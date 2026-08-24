# CLAUDE.md — adjourn

Untimed correspondence chess as a Freenet decentralized app.

| crate | role |
|---|---|
| `common/` (`adjourn-core`) | the state algebra. **No Freenet dependencies** — the consistency model is testable standalone, and CI asserts the dependency graph stays clean. |
| `contracts/adjourn-contract/` | the `ContractInterface` adapter. Bytes in, bytes out; no logic of its own. |
| `delegates/adjourn-delegate/` | holds per-game signing keys; enforces one signature per (game, ply). |
| `cli/` (`adjourn-cli`) | the `adjourn` headless CLI. Loads the compiled contract and delegate WASM off disk, speaks the node's WebSocket API, and drives `key`/`invite`/`game`/`move`/`show`/`resign`/`draw`. Nearly every flow that touches the delegate or contract lives in `adjourn_cli::session` and is exercised there against `FakeNode`; the one exception is `ListGames`, which `main.rs` sends directly (it backs both `key list` and `game list`, which render it differently). Otherwise `main.rs` is parse-dispatch-render only. |

`validate_state` → `all_valid`, `update_state` → `merge`, `summarize_state` →
`summarize`, `get_state_delta` → `delta_against`.

## Read before changing anything

- The Freenet whitepaper: https://freenet.org/whitepaper/ — especially §4
  (contract algebra, summary/delta) and §7.4 (what the platform can't do).
  Deliberately not vendored here; fetch your own copy.
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
   forfeit > **resignation** > board result > **draw claim** > draw agreement.
   Any fixed order works; it just has to be identical on every peer.
   Resignation sits above the board result because `Resign` is unanchored and
   unconditional — it names no position, so there is no ply at which it stops
   applying. Ranking the board first let a player resign and then play on to a
   mate, and be awarded the win by their own resigned game. Test:
   `resignation_outranks_a_later_board_result`.

   The board result outranks a draw claim because the claimant is by definition
   the player to move — so if that position is checkmate, the claimant is the
   player who has just been mated. Ranking the claim first would let a mated
   player draw their way out of a loss. Tests: `a_claim_does_not_disturb_a_checkmate`,
   `a_valid_claim_outranks_a_pending_draw_agreement`.

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

10. **The eviction group key includes the record's `kind`.**
    Merge keeps only the K smallest ids per `(signer, kind, ply)` group. Were
    kind left out, a player could flood `DrawOffer` records at ply N and evict
    their own `Move` records at ply N — including both halves of a double-sign
    fraud proof. Grouping per *signer* is what makes eviction safe against an
    opponent: your records only ever compete with your own. Tests:
    `flooding_draw_offers_cannot_evict_a_move_at_the_same_ply`,
    `a_spammer_cannot_evict_the_opponents_records`.

## Persisted-record versioning

The network encoding needs no version field: the contract key is
`hash(code, params)` and the encoding lives in that code, so two peers syncing
one contract instance are running byte-identical WASM by construction and
cannot disagree about the format. A version prefix there would cost bytes on
every message to detect a mismatch that cannot occur.

The delegate's secret store is the opposite case. `RegisterDelegate` carries a
`predecessors` list and the node copies LOCAL-scope secrets forward into the new
generation's namespace, so a future delegate **will** read records this one
wrote. `GameRecord` therefore carries `format: u8` (`GAME_RECORD_FORMAT`,
currently 2), checked at the top of both `decide_bind` and `decide_sign` before
any other field is trusted.

The failure being defended against is a decode *success*, not a decode error: a
later version that adds a `#[serde(default)]` field would let serde deserialize
an old record with `last_signed_ply` defaulted to 0, silently resetting the
double-sign guard on a real in-progress game. Bump the constant whenever the
layout changes and teach the reader to migrate the old shape — never widen the
check. Tests: `a_record_from_another_format_cannot_be_signed_against`,
`the_format_check_precedes_every_other_check`.

## Wire format

Everything here feeds `Record::id()` and the signing payload, so **any change
rotates every id and invalidates every signature**. Treat this table as frozen
once a game exists on the network.

| Rust | wire key |
|---|---|
| `Record.body` / `.signer` / `.sig` | `b` / `k` / `s` |
| `Body::Move` / `Resign` / `DrawOffer` / `DrawAccept` | `m` / `r` / `o` / `a` |
| `Move.ply` / `.parent` / `.uci` | `p` / `t` / `u` |
| `DrawOffer.ply` / `.at` | `p` / `t` |
| `DrawAccept.ply` / `.offer` | `p` / `o` |
| `Body::DrawClaim` | `c` |
| `DrawClaim.ply` / `.at` | `p` / `t` |

`GameState` is a **sequence** of records, not a map — the id is recomputed on
decode. Decode **rejects duplicate ids**: an honestly-serialized state cannot
contain them (map keys are unique), so a duplicate means crafted bytes, and
`decode` has no `params` with which to tell an honest signature from a forgery.
Refusing beats guessing.

`Summary` is one packed byte string of 64-byte `id ‖ digest` entries in id
order. A payload that is not a whole number of entries, or that repeats an id,
is refused.

Byte fields carry `#[serde(with = "serde_bytes")]` so CBOR emits byte strings
rather than arrays of integers. A `[u8; 32]` costs 34 bytes with it and ~55
without.

## Anti-goals for v1

Do not add: timers or clocks (self-reported timestamps are unenforceable —
your parent's timestamp is set by your opponent, so "I moved one second after
you" is always claimable); matchmaking; ratings; wagers or stakes; a lobby
contract. Scope is two players who exchange params out of band.

If ratings are ever added: **Elo is order-dependent** and will not converge.
Use a global-fit method (Whole-History Rating, Bradley–Terry MLE) that is a
pure function of the result set.

## Known issues, unresolved

- **State growth: bounded, at a cost.** Merge keeps the K smallest ids per
  `(signer, kind, ply)` group — K=2 for moves, K=1 for draw records — and
  `MAX_PLY = 4096` bounds the number of groups. Worst case is ~41,000 records
  or ~6.4 MB, against a normal game's 1100 bytes. Top-K distributes over merge
  (`topK(topK(A) ∪ topK(B)) = topK(A ∪ B)`), so the rule is idempotent and the
  monoid survives.

  A ply-window rule ("drop moves beyond chain length + 1") is still **not**
  safe: chain length is shorter in a partial state, so a peer would evict
  records the merged state needs.

- **Eviction gives any player an unconditional way to void a game.** Eviction
  must sort blind by id: legality depends on the position, which depends on
  the chain, which depends on which records are present, so a legality-aware
  rule would evict different records in a partial state and peers would
  diverge. But blind-by-id cuts both ways. A player who has already played a
  ply can sign two lower-id junk `Move` records at that same ply — no
  cheating required, no double-sign involved — and their own real move falls
  out of the top-K. The chain stops one ply short and the game ends with no
  result. This is not a narrow loophole for a cheater dodging a forfeit; it is
  available to either player, at any ply, at will, and no value of K prevents
  it — the only rule that would is legality-aware, which is exactly the
  divergence trap above.

  The double-sign forfeit inherits the same hole: bury the second signature
  under lower-id illegal records at that ply and `walk` finds no forfeit
  candidate either, just a stalled chain.

  The trade is accepted because nothing new is gained by it: the outcome is a
  stalled game with no decision, which is what walking away already produces.
  No win is ever stolen — at worst a loss becomes a no-result. A
  witness-published fraud proof embedding both offending records would close
  the double-sign case, but defining fraud without reference to a position
  collides with invariant 8's castling case. Test:
  `a_buried_double_sign_stalls_instead_of_forfeiting`.

- **The outcome is not monotone.** A strict superset can move the projection
  *down*: a decided game plus one extra record (a late-published double-sign
  fraud proof) becomes a forfeit at the fork point, with `fen` and `ply` rewound
  and the opposite winner. Peers holding the same set still agree, so this is
  not a convergence bug, and it is not exploitable — double-signing only ever
  forfeits the signer. But a withheld fraud proof can reverse a displayed
  result long after the fact. The UI should show the full chain rather than the
  truncated `fen` after a forfeit.
  Test: `superset_reverses_the_outcome_and_rewinds_the_board`.

- **Threefold and fifty-move are claims, and `DrawClaim` is how they are
  made.** FIDE makes these a *claim* (9.2, 9.3), not an automatic result.
  `Status.repetitions` and `Status.halfmove_clock` expose the grounds, and a
  `DrawClaim` record anchored to the head cashes them. The claim carries no
  kind: projection knows the repetition count and halfmove clock at the head,
  so it checks whether either ground holds and reports which fired. Only the
  player to move may claim — letting the idle player claim would let the player
  to move void it by moving, which is the race that anchoring to the head
  exists to avoid. The automatic rules (fivefold 9.6.1, seventy-five-move
  9.6.2) still fire in `walk` and end the game with no claim needed. Tests:
  `a_threefold_claim_at_the_head_draws_the_game`, `a_stale_claim_is_ignored`,
  `only_the_player_to_move_may_claim`.

- **The 75-move threshold has no end-to-end test.** The rule is implemented in
  `walk` next to the fivefold check, but a 150-halfmove line that avoids
  fivefold first is awkward to construct by hand. Fivefold is covered.

- **CBOR encoding: done.** A 7-record game is 1100 bytes (157 a record), down
  from 2494 (356). Three changes got there, all breaking, all landed in one
  pre-1.0 pass:
  `serde_bytes` on every byte field; `GameState` encoding as a bare *sequence*
  (the map key was `rec.id()`, derivable, 34 bytes a record of pure
  duplication); and short `#[serde(rename)]` wire keys on `Record`/`Body`.
  `Summary` — which rides on EVERY sync round — is now a newtype packing to
  exactly 64 bytes an entry (`id ‖ digest`) instead of ~110.

  `GameParams` and the delegate API types are deliberately left readable: the
  line is *rename what repeats per record on the wire; leave one-off and local
  types legible*. Params are 104 bytes once per contract, and that is the thing
  you squint at when debugging a contract key.

- **`make_move` round-trips through FEN** to re-derive the position, which
  loses history. Now that `walk` tracks repetitions, this also means
  `make_move` cannot see them — it relies on `project` having already decided
  the game is over. Consider returning the `Chess` from `project` instead.

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

`delegates/adjourn-delegate/` holds the per-game signing key and enforces one
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
  decision logic in `adjourn-core` means it is still tested on every platform;
  verify the delegate itself only with `--target wasm32-unknown-unknown`.
- **The off-wasm `rand_bytes` stub returns zeros silently.** It exists so
  `adjourn-core` builds on the host at all, but a zeroed "random" draw would
  produce a signing key an attacker could guess. `classify_host_entropy` takes
  two independent draws and treats a dead (all-zero, or identical) source as a
  refusal rather than trusting the first draw at face value.
- **The `freenet-main-delegate` feature must be declared** by the delegate
  crate, exactly like `freenet-main-contract` — the `#[delegate]` macro
  expands to code gated on it.
- **The handlers in `lib.rs` are host-testable via the `SecretStore` trait**
  (`secrets.rs`). `handle_create_game_key`, `handle_bind_game`, `handle_sign`,
  and `handle_list_games` are generic over `S: SecretStore` rather than tied to
  `DelegateCtx`, so `cli/src/fake.rs`'s `FakeNode` runs the real handler logic
  against an in-memory `MemoryStore` off-wasm, with no `wasm32-unknown-unknown`
  build or wasmtime instance in the loop. Only the `DelegateCtx` impl of
  `SecretStore` (talking to the real secret store host import) needs the wasm
  target.
- **`GameRecord.origin` is `Option<[u8; 32]>`, not `Option<MessageOrigin>`**
  (`common/src/delegate_policy.rs`) — what is persisted is the origin's
  derived contract-instance id, not the enum. `Option<MessageOrigin>` is one
  layer up: the parameter type the runtime hands the delegate's `lib.rs`
  handlers, which `origin_id()` converts to `Option<[u8; 32]>` before it ever
  reaches `delegate_policy` or gets stored. The origin binds a bound game to
  whichever id created it, and `handle_list_games` filters by
  `load_owner(store, &label) == origin` so one origin cannot enumerate
  another's labels. A CLI client has no `MessageOrigin` at all — the node
  passes `origin: None` for a direct WS-API caller — so
  making the field optional was required, not speculative: with a non-optional
  field every CLI-issued bind and signature would have been refused outright.
  The corollary: for a CLI-bound game the origin check provides no isolation
  between callers, because all CLI callers present the same `None`. The real
  boundary for a CLI-bound game is that the node's WebSocket API is
  loopback-only — see "Runtime assumptions, verified" below, which records
  this confirmed against a live node.

## Testing

`cargo test --workspace --locked` — 131 tests: 93 in `adjourn-core` (23 algebra
tests, 31 adversarial tests, and 39 delegate-policy tests), 16 contract tests,
9 delegate adapter tests, and 13 CLI integration tests. The algebra tests are
the point; they run randomized partitions and delivery orders. Keep them
green. New state-shape features need a corresponding law test, not just a
happy-path test.

- `common/tests/algebra.rs` (23) — the monoid laws and the original
  adversarial cases.
- `common/tests/adversarial.rs` (31) — convergence attacks, outcome
  precedence, and the chess edges (promotion, underpromotion, castling
  notation, en passant, repetition, top-K eviction, `MAX_PLY`, and draw
  claims).
- `common/tests/delegate_policy.rs` (39) — the delegate's pure decision
  functions: bind/sign refusals, entropy classification and mixing, the
  ply-0 sentinel guard, and wire round-trips for `Request`/`Response`/
  `GameRecord`/`GameSummary` through CBOR. Runs on any platform.
- `contracts/adjourn-contract/tests/interface.rs` (16) — the adapter: byte
  encodings, empty-state cases, two peers converging in one round through the
  real interface, and that chess legality is NOT a validity condition.
- `delegates/adjourn-delegate/tests/adapter.rs` (9) — CI-only. Two groups:
  the secret-store key namespaces never collide (and a crafted label cannot
  forge another namespace's prefix), plus dispatch tests that drive
  `adjourn_delegate::handle` directly — a key created and listed, a label
  hijack attempt from a different origin refused as `WrongOrigin`, and a
  double-sign attempt refused through the real dispatch path, not just the
  policy layer beneath it.
- `cli/tests/` (13, across `fake_node.rs` 2, `full_game.rs` 1, `invite.rs` 4,
  `moves.rs` 4, `setup.rs` 2) — the CLI's `session.rs` flows run against
  `FakeNode` (real contract and delegate code, in-memory transport): both
  players deriving the same contract, a build mismatch refused loudly, a
  full scholar's-mate game end to end, out-of-turn moves failing before
  signing, and a double-sign attempt refused by the delegate. Several of
  these read the compiled contract WASM off disk and skip themselves if it
  is absent locally -- but panic instead if `CI` is set, so a skip can never
  masquerade as a pass in CI (see `cli/tests/common/mod.rs::contract_wasm`).

### Check against the network's own verifier

`fdev conformance` runs the *same* verifier freenet-core runs, over a corpus of
real states, and it catches things our own tests structurally cannot:

```sh
fdev conformance   --wasm target/wasm32-unknown-unknown/release/adjourn_contract.wasm   --params params.bin --state state_a.bin --state state_b.bin ...
```

It found `self_delta_empty` — `get_state_delta` returned a CBOR-encoded empty
list (one byte, `0x80`) instead of zero bytes, so freenet-core's "empty delta ->
skip" broadcast path could never fire. Our tests missed it because they assert
on the DECODED delta, which really is empty; the network reads the encoded
length. That is the same failure that drove River's 2026-07-25 incident, where
the room contract took 63.7% of network-wide byte-weighted broadcast work.

`summarize_state` and `get_state_delta` both call `filter_valid(&params)`
before summarizing or diffing, for the same never-settles reason.
`validate_state` is deliberately permissive (invariant 1: rejecting content is
forbidden, since a required-valid state lets one player destroy a game by
signing garbage), so a peer can be handed a state that is over-K — via a
crafted PUT, or a delta assembled from fragments no single honest peer ever
held. Without normalizing on read, that peer keeps summarizing and diffing
against records eviction has already discarded: it re-offers the same
evicted-away records every sync round, forever, and its counterpart never
reports itself in sync — the identical shape as `self_delta_empty` above,
where the network read encoded bytes the code path never produced. Both bugs
are "the decoded value looks fine; the peer never converges because of what
actually got compared or sent."

Feed it partial and adversarial states, not just happy games -- the merge laws
only bite when peers hold different fragments. Current status: 348 cases, 348
held, 0 violations.

The `hazmat` dev-dependency on ed25519-dalek exists so the tests can forge a
*second valid* signature over one body — the case invariant 4 is about. It is a
dev-dependency only and never enters the contract build.

`merge` verifies signatures, so the randomized law tests take ~80s. That is the
honest cost of invariant 3; the fast path in `absorb` skips verification only
when the record is already held byte-for-byte.

## Runtime assumptions, verified

Against a live `freenet 0.2.130` node, following `docs/runbook-two-nodes.md`,
2026-08-24:

- **`MessageOrigin` is NOT populated for a CLI client.** `adjourn invite
  accept` successfully bound a game and `adjourn game list` showed it
  afterward, which is only possible because the delegate accepts `origin:
  None` (see "the handlers are host-testable via `SecretStore`" above). This
  confirms the change making `GameRecord.origin` an `Option` was necessary,
  not speculative — with a non-optional field every bind and every signature
  from the CLI would have been refused. For CLI-bound games the origin check
  therefore provides no isolation between callers; the real boundary is the
  node's loopback-only WebSocket API, not the delegate's origin field.
- **The delegate and contract execute in wasmtime.** `adjourn init` registered
  the delegate as `EiLsNrWwx33pKjk9JRfpYYAy3KiPrLum4hYtZLZQJWwy` and `adjourn
  key new` returned a key, so both modules instantiate and run under the real
  node. No unresolved-import failure, which is what `getrandom` reaching
  either dependency graph would have produced.
- **`freenet_rand` IS provided and supplies real entropy, on this node
  version.** After the Task 10 output fix, `adjourn key new` was re-run
  against the same live node:

  ```
  $ adjourn key new --label entropy-probe
  entropy-probe: 9ZVkjHbpdPvKkvrfwJCbLC2JrPtVwhWQaGmBAdBW6KKd  entropy: HostBacked

  $ adjourn game list
  bob  Black  contract FMyx3cuVMktTLPmw9mcQFyD46bqxMJbKWkddTRLPa6bz  last signed ply 0  host-backed entropy
  ```

  `HostBacked` means `classify_host_entropy`'s two-draw liveness check (see
  "Delegate" above) saw two different, non-zero draws from the host import —
  so `freenet_rand` both resolves and returns genuine randomness, rather than
  the all-zeros a dead source would produce.

  This is the difference between the two security properties the design
  distinguishes for a freshly generated per-game key:
  - **`HostBacked`** — the key is unpredictable even to a UI that is hostile
    at the moment of creation. The strong property, and the one that holds
    here.
  - **`Degraded`** (had it come back this way) — the key is derived solely
    from caller-supplied entropy, so a caller that retained its own
    contribution could recompute the private key. Safe only against a UI
    compromised *later*, not at creation time.

  The `Degraded` path and its warning stay exactly as written: it is a real
  fallback for a node/host that behaves differently, and the fail-closed
  branch (no host entropy *and* no caller entropy) is what stops a key ever
  being minted from nothing. This result is one measurement, on one node
  version, on one date — `freenet 0.2.130`, 2026-08-24 — not a guarantee
  about every node version. That is why it is recorded with both attached
  rather than asserted as a permanent property of the platform.
- **A node started with plain `nohup` does not survive the shell; `setsid
  nohup ... < /dev/null &` does.** Confirmed twice now: once when the bind
  went through in the first live run, and again when a node started this way
  in an earlier session was found still running after the session that
  started it had ended. See `docs/runbook-two-nodes.md`.

## Roadmap

1. `ContractInterface` wrapper (`validate_state` → `all_valid`, `update_state`
   → `merge`, plus `summarize_state` / `get_state_delta`)
2. Delegate holding the per-game signing key; UI never sees it
3. UI over the WebSocket API: `get`, `subscribe`, `update` — done for
   everything except `watch` (see `docs/runbook-two-nodes.md`); `watch` needs
   a streaming `NodeClient` method that does not exist yet.
