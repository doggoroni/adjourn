# CLAUDE.md — freenet-chess

Untimed correspondence chess as a Freenet decentralized app.

| crate | role |
|---|---|
| `common/` (`chess-core`) | the state algebra. **No Freenet dependencies** — the consistency model is testable standalone, and CI asserts the dependency graph stays clean. |
| `contracts/chess-contract/` | the `ContractInterface` adapter. Bytes in, bytes out; no logic of its own. |
| `delegates/chess-delegate/` | holds per-game signing keys; enforces one signature per (game, ply). |

`validate_state` → `all_valid`, `update_state` → `merge`, `summarize_state` →
`summarize`, `get_state_delta` → `delta_against`.

## Read before changing anything

- `freenetwhitepaper.pdf` — especially §4 (contract algebra, summary/delta) and
  §7.4 (what the platform can't do)
- Tutorial: https://freenet.org/build/manual/tutorial/
- Contract interface: https://freenet.org/build/manual/contract-interface/
- Reference app (read this): https://github.com/freenet/river
- `freenet-scaffold`: https://github.com/freenet/freenet-scaffold

## Platform constraints that drive the design

Freenet gives eventual consistency per contract, with no global ordering.
Contract state must form an **idempotent commutative monoid**: merge is
associative, commutative, idempotent, with an identity. The runtime **cannot
verify this**. A bad merge doesn't error — replicas silently fail to converge.

Merge is **total**. It cannot reject. Anything requiring "one of these two
updates loses" is outside the model. Contract state is **public** — no secrets.
The contract key is the **hash of the compiled WASM**, so a dependency bump
rotates the app's address.

## The design in one paragraph

State is an unordered **set of signed records**, merged by **union**. That is
the whole consistency story, and it contains no chess knowledge. Ordering comes
from a **parent-hash chain** — each move commits to its predecessor's hash — so
strict sequencing is enforced by cryptography, not by the network. The board
position is a **pure projection** of the merged set and is never stored.

At any ply only one player holds a key that can produce a valid record, so
concurrent writes are impossible rather than merely resolved.

## INVARIANTS — do not change without explicit discussion

These look like bugs. They are not.

1. **`validate_state` must NOT check chess legality.**
   `Record::verify()` only checks that a record is signed by one of the two
   players. Illegal moves are *ignored at projection*. If illegality made the
   whole state invalid, either player could permanently destroy a game by
   signing one garbage move — every honest peer would reject the entire state.
   Test: `illegal_move_is_ignored_not_fatal`.

2. **`Record::id()` must NOT cover the signature.**
   Id is `H(signer ‖ body)`. If the id covered the sig, the same statement
   signed twice would occupy two slots and merge would stop being idempotent.
   Test: `signature_malleability_does_not_split_records`.

3. **Id collisions tiebreak on `min(sig)` *among records that verify*.**
   First-writer-wins is order-dependent and breaks commutativity. But the
   tiebreak must never run against an unverified record: an all-zero signature
   is lexicographically minimal and forgeable by anyone who has seen the
   record, so a raw byte comparison lets a forgery evict the honest move. That
   is why `merge` takes `params` — the monoid is over the set of *valid*
   records, and absorption has to be able to tell which those are. It is also
   what makes `merge` and `filter_valid` commute.
   Tests: `forged_signature_cannot_evict_the_valid_record`,
   `merge_and_filter_commute`.

4. **The summary carries `H(sig)` alongside each id, not just the id.**
   Follows from invariant 2. Ids exclude the signature, and ed25519 does not
   pin the nonce, so a player running their own signer can produce two *valid*
   signatures over one body. A set-of-ids summary reports two peers holding
   different bytes as already in sync, and they never converge — a direct
   violation of whitepaper Property 1 (§4.4). Carrying the digest means both
   peers offer, both absorb, and both land on the same `min(sig)` winner in one
   round. Tests: `two_valid_signatures_on_one_body_converge_in_one_round`,
   `property_1_sync_soundness_holds_on_a_signature_collision`.

5. **`BTreeMap`, never `HashMap`.**
   Canonical iteration order gives byte-identical serialization across peers.
   Test: `round_trips_through_cbor` asserts canonical encoding.

6. **Every signature binds `game_id`.**
   Otherwise a move replays from one game into a rematch between the same two
   players. Test: `moves_do_not_replay_across_games`.

7. **Outcome precedence is fixed and documented:**
   forfeit > **resignation** > board result > draw agreement. Any fixed order
   works; it just has to be identical on every peer. Resignation sits above the
   board result because `Resign` is unanchored and unconditional — it names no
   position, so there is no ply at which it stops applying. Ranking the board
   first let a player resign and then play on to a mate, and be awarded the win
   by their own resigned game. Test:
   `resignation_outranks_a_later_board_result`.

8. **One move has exactly one spelling.**
   shakmaty accepts both `e1g1` and `e1h1` for the same castling move. Two
   spellings are two bodies with two ids for one move, which `walk` would read
   as a double-sign and forfeit an honest player over a notation mismatch.
   `make_move` signs the canonical form, and `walk` collapses candidates by the
   move they resolve to before counting. Tests:
   `make_move_canonicalises_castling_notation`,
   `two_spellings_of_one_castling_move_do_not_forfeit`.

9. **Draw offers are bound to the head.**
   An agreement counts only while `DrawOffer.at` is still the head of the
   chain. A player offers a draw right after their own move, so it is the
   opponent's turn while the offer stands — the acceptor is the only player who
   can advance the head, so an acceptance can be voided only by the acceptor's
   own subsequent move, never by the offerer. When the acceptor both accepts
   and moves at the same head, the move wins: those two orderings produce
   *identical record sets*, so no pure function can tell them apart, and
   letting the move win never ends a game someone is still playing. Tests:
   `stale_draw_offer_is_ignored`, `draw_offer_at_the_current_head_is_accepted`,
   `accepting_and_then_moving_voids_your_own_acceptance`.

## Anti-goals for v1

Do not add: timers or clocks (self-reported timestamps are unenforceable —
your parent's timestamp is set by your opponent, so "I moved one second after
you" is always claimable); matchmaking; ratings; wagers or stakes; a lobby
contract. Scope is two players who exchange params out of band.

If ratings are ever added: **Elo is order-dependent** and will not converge.
Use a global-fit method (Whole-History Rating, Bradley–Terry MLE) that is a
pure function of the result set.

## Known issues, unresolved

- **Unbounded state growth.** A player can sign unlimited structurally-valid
  but illegal move records. Bounded by the two keys, but nothing caps it. Any
  eviction rule must be a pure function of the merged set.

  The rule that works is **top-K by id within each `(signer, ply)` group**. The
  K smallest ids of `A ∪ B` are always a subset of (K smallest of A) ∪ (K
  smallest of B), so filtering distributes over merge and the rule is
  idempotent. K=2 preserves the double-sign fraud proof. Grouping *per signer*
  is what makes it safe: your own move only ever competes with your own
  records, so an opponent cannot evict your move, and a player who spams
  themselves out of a legal move merely stalls their own game.

  A ply-window rule ("drop moves beyond chain length + 1") is **not** safe:
  chain length is shorter in a partial state, so a peer would evict records the
  merged state needs. `filter(A) ∪ filter(B) ≠ filter(A ∪ B)`. Divergence.

- **The outcome is not monotone.** A strict superset can move the projection
  *down*: a decided game plus one extra record (a late-published double-sign
  fraud proof) becomes a forfeit at the fork point, with `fen` and `ply` rewound
  and the opposite winner. Peers holding the same set still agree, so this is
  not a convergence bug, and it is not exploitable — double-signing only ever
  forfeits the signer. But a withheld fraud proof can reverse a displayed
  result long after the fact. The UI should show the full chain rather than the
  truncated `fen` after a forfeit.
  Test: `superset_reverses_the_outcome_and_rewinds_the_board`.

- **Threefold and fifty-move are reported, not forced.** FIDE makes these a
  *claim* (9.2, 9.3), and there is no claim record type. `Status.repetitions`
  and `Status.halfmove_clock` expose them so a UI can offer the claim; the
  automatic rules (fivefold 9.6.1, seventy-five-move 9.6.2) fire in `walk` and
  do end the game. Adding a `DrawClaim` body is a live design question.

- **The 75-move threshold has no end-to-end test.** The rule is implemented in
  `walk` next to the fivefold check, but a 150-halfmove line that avoids
  fivefold first is awkward to construct by hand. Fivefold is covered.

- **`Status.ignored` is imprecise** — it's `len - chain.len()`, so it counts
  resign/draw records as "ignored".

- **CBOR encoding is ~2.4× larger than necessary.** 2494 bytes for 7 records
  against ~150 bytes of actual content, because serde encodes `[u8; 32]` and
  `Vec<u8>` as CBOR *arrays of integers* rather than byte strings.
  `serde_bytes` roughly halves it. Decide this **before launch**: `body_bytes`
  feeds both `Record::id()` and the signing payload, so changing the encoding
  rotates every id and invalidates every signature.

- **`make_move` round-trips through FEN** to re-derive the position, which
  loses history. Now that `walk` tracks repetitions, this also means
  `make_move` cannot see them — it relies on `project` having already decided
  the game is over. Consider returning the `Chess` from `project` instead.

- **`ply: u16` overflows.** `walk` does `ply += 1` with no bound; debug builds
  panic at 65535. Unreachable in practice now that the automatic draw rules
  terminate games, but there is still no explicit cap.

- **Abandonment.** No timers means a player who stops leaves a contract that
  goes cold and gets evicted. Current answer: let it die, UI keeps a local PGN.

## Reproducible builds — the contract key

The contract key is the hash of the compiled WASM, so **anything that changes
the emitted bytes silently rotates the app's address** and orphans every game in
progress. Four things are pinned to prevent that:

1. **Exact `=` dependency pins** in the workspace `Cargo.toml`. A caret range
   lets `cargo update` pull a patch release and change the bytes.
2. **`Cargo.lock` committed**, and `--locked` on every build.
3. **`rust-toolchain.toml` pinned to an exact version** (1.97.1). rustc version
   changes the codegen.
4. **`--remap-path-prefix`**, applied by `scripts/build-contract.sh`. Without
   it the WASM embeds `$HOME/.cargo/registry` through dependency panic
   locations, so the key would depend on *who built it*. Cargo's `trim-paths`
   would be tidier but is unstable in 1.97.1 and would force nightly.

**Build the contract only via `scripts/build-contract.sh`.** A bare
`cargo build --release` embeds your home directory and produces a different,
unshippable key. The script fails loudly if a build path leaked.

Reproducibility holds **within one OS**. The remap removes machine-specific
prefixes, but the path separator style (`\` vs `/`) still differs, so Windows
and Linux builds differ in bytes. `ubuntu-latest` in CI is the reference
platform; local builds on other OSes are for development only.

See https://freenet.org/build/manual/upgrading-contracts/ — River derives its
contract key as `blake3(code_hash ‖ owner_key)` so invite links survive
upgrades; worth copying that pattern if game URLs ever need to outlive a
contract version.

### Contract crate gotchas

- **No `rand` or `getrandom`, ever.** wasmtime has no getrandom backend on
  `wasm32-unknown-unknown`; the crates emit wasm-bindgen placeholder imports
  that cannot be resolved, and the contract fails to instantiate
  (freenet/river#241). CI asserts the graph stays clean. This is why
  `ed25519-dalek` is `default-features = false` with no `rand_core`: the
  contract only ever *verifies*.
- **`UpdateData` is `#[non_exhaustive]`.** The catch-all arm must return
  `Err(ContractError::InvalidUpdate)`, never `unreachable!()` — a panic inside
  contract WASM kills the runtime for the contract and surfaces as an opaque
  execution error.
- **Empty state is valid, not malformed.** A contract is PUT before either
  player moves, and peers summarize against a state they just created. An empty
  *summary* likewise means "I have nothing", not "I am up to date".
- **The `freenet-main-contract` feature must be declared** by the contract
  crate; the `#[contract]` macro expands to code gated on it.
- **Host builds need a full linker toolchain.** `freenet-stdlib` depends on
  `tracing-subscriber` unconditionally, which pulls `windows-sys` on Windows.
  The wasm32 target does not.

## Delegate

`delegates/chess-delegate/` holds the per-game signing key and enforces one
signature per `(game, ply)` so a compromised or buggy UI cannot double-sign a
move out from under the player. As with the contract, **build only via
`scripts/build-delegate.sh`** — a bare `cargo build --release` embeds
machine-specific paths and produces a different, unshippable key.

- **All policy lives in `common/src/delegate_policy.rs`, and it is pure** — no
  `DelegateCtx`, no secret-store I/O. That is deliberate, not just tidy: the
  delegate crate **cannot be host-compiled on Windows**, for the same
  `freenet-stdlib` → `tracing-subscriber` → `windows-sys` chain as the
  contract, except the delegate has no workaround — `windows-sys` needs a full
  mingw `binutils` that the rustup `gnu` toolchain does not ship. Keeping the
  decision logic in `chess-core` means it is still tested on every platform;
  verify the delegate itself only with `--target wasm32-unknown-unknown`.
- **The off-wasm `rand_bytes` stub returns zeros silently.** It exists so
  `chess-core` builds on the host at all, but a zeroed "random" draw would
  produce a signing key an attacker could guess. `classify_host_entropy` takes
  two independent draws and treats a dead (all-zero, or identical) source as a
  refusal rather than trusting the first draw at face value.
- **The `freenet-main-delegate` feature must be declared** by the delegate
  crate, exactly like `freenet-main-contract` — the `#[delegate]` macro
  expands to code gated on it.

## Testing

`cargo test --workspace --locked` — 76 tests: 59 in `chess-core` (31 algebra
tests plus 28 delegate-policy tests), 13 contract tests, and 4 delegate adapter
tests. The algebra tests are the point; they run randomized partitions and
delivery orders. Keep them green. New state-shape features need a
corresponding law test, not just a happy-path test.

- `common/tests/algebra.rs` (12) — the monoid laws and the original
  adversarial cases.
- `common/tests/adversarial.rs` (19) — convergence attacks, outcome
  precedence, and the chess edges (promotion, underpromotion, castling
  notation, en passant, repetition).
- `common/tests/delegate_policy.rs` (28) — the delegate's pure decision
  functions: bind/sign refusals, entropy classification and mixing, the
  ply-0 sentinel guard. Runs on any platform.
- `contracts/chess-contract/tests/interface.rs` (13) — the adapter: byte
  encodings, empty-state cases, two peers converging in one round through the
  real interface, and that chess legality is NOT a validity condition.
- `delegates/chess-delegate/tests/adapter.rs` (4) — CI-only: the secret-store
  key namespaces never collide.

The `hazmat` dev-dependency on ed25519-dalek exists so the tests can forge a
*second valid* signature over one body — the case invariant 4 is about. It is a
dev-dependency only and never enters the contract build.

`merge` verifies signatures, so the randomized law tests take ~80s. That is the
honest cost of invariant 3; the fast path in `absorb` skips verification only
when the record is already held byte-for-byte.

## Roadmap

1. `ContractInterface` wrapper (`validate_state` → `all_valid`, `update_state`
   → `merge`, plus `summarize_state` / `get_state_delta`)
2. Delegate holding the per-game signing key; UI never sees it
3. UI over the WebSocket API: `get`, `subscribe`, `update`
