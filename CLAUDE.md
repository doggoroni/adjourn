# CLAUDE.md — adjourn

Untimed correspondence chess as a Freenet decentralized app.

| crate | role |
|---|---|
| `common/` (`adjourn-core`) | the state algebra. **No Freenet dependencies** — the consistency model is testable standalone, and CI asserts the dependency graph stays clean. |
| `contracts/adjourn-contract/` | the `ContractInterface` adapter. Bytes in, bytes out; no logic of its own. |
| `delegates/adjourn-delegate/` | holds per-game signing keys; enforces one signature per (game, ply). |
| `client/` (`adjourn-client`) | the game flows (`session.rs`, `invite.rs`), transport-independent — generic over `node::NodeClient` rather than tied to any one WebSocket implementation. `FakeNode` (real contract and delegate code, in-memory transport) lives here too, behind a default-on `fake` feature the UI turns off. |
| `cli/` (`adjourn-cli`) | the `adjourn` headless CLI: the tungstenite `NodeClient` impl (`ws.rs`), argument parsing, and rendering. It drives `key`/`invite`/`game`/`move`/`show`/`resign`/`draw`/`watch` by calling into `adjourn_client::session`; it no longer holds the flows itself. The one exception is `ListGames`, which `main.rs` sends directly (it backs both `key list` and `game list`, which render it differently) — otherwise `main.rs` is parse-dispatch-render only. |
| `ui/` (`adjourn-ui`) | the Dioxus web UI. `board.rs` is a pure projection of `adjourn_core::project`'s output onto squares (never touches the network); `node.rs` is the browser `NodeClient` impl; `conn.rs` is the single coroutine that owns that client and serialises every command a screen sends; `app.rs`/`views/` are the shell and the four screens. Depends on `adjourn-client` with `default-features = false` so the contract and delegate crates — and their WASM toolchains — never enter the wasm build. The library `include_bytes!`s both compiled WASM modules directly (`ui/src/main.rs` calls `dioxus::launch(adjourn_ui::app::App)`), because a browser cannot read them off disk at runtime the way the CLI reads them off disk at startup — which pins both the contract's and the delegate's keys into whatever build of the UI you ship. |

`validate_state` → `all_valid`, `update_state` → `merge`, `summarize_state` →
`summarize`, `get_state_delta` → `delta_against`.

`adjourn-client` exists to be shared, not to be reused for its own sake: both
players must derive byte-identical `GameParams`, or they land on different
contract ids and each sees a game the other never joins, with **no error
anywhere** to signal the split. One implementation of the flows is the only way
to be sure both sides compute params the same way — and a browser cannot reach
those flows while they live in a crate that pulls in `tokio-tungstenite`, which
is why they were pulled out of `cli/` and into a crate with no transport
dependency of its own. `FakeNode` rides along behind the `fake` feature
(default-on) so the UI can build with `default-features = false` and keep the
contract and delegate crates — and their WASM toolchains — out of its own
build; co-building them in one cargo invocation is also how feature
unification could change the *contract's* emitted bytes, which the
reproducible-builds section below treats as unacceptable for the actual
shipped contract.

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
   ONE illegal move is ignored; only *two or more* `Move` records at one ply
   are fatal, and that rule (invariant 11) counts records without ever asking
   whether any of them is legal. Tests: `illegal_move_is_ignored_not_fatal`,
   `a_legal_and_an_illegal_move_at_one_ply_forfeit`.

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

8. **One move has exactly one spelling — and signing two now forfeits.**
   shakmaty accepts both `e1g1` and `e1h1` for the same castling move. Two
   spellings are two bodies with two ids for one move.

   This invariant used to promise that such a pair was *not* read as a
   double-sign: `walk` collapsed candidates by the move they resolved to
   before counting. Invariant 11 reopened it deliberately. The double-sign
   forfeit is now structural — it counts `Move` records per `(signer, ply)`
   and never consults the position — and a position-free rule cannot tell
   two spellings of one castling move apart. So signing both forfeits, and
   `walk`'s collapse logic is gone (it had become unreachable).

   The obligation therefore moves entirely onto the writers, and the stock
   stack discharges it twice over: `make_move` signs only the canonical
   spelling, and the delegate refuses a second signature at an already-signed
   ply. Only a third-party client signing raw bodies could produce both, and
   it would forfeit its own user over notation. That was the accepted price
   of closing retroactive move substitution. Tests:
   `make_move_canonicalises_castling_notation`,
   `two_spellings_of_one_castling_move_now_forfeit`.

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

11. **The double-sign forfeit is STRUCTURAL, not legality-based.**
    A signer holding two or more `Move` records at one ply forfeits. The rule
    counts records over `(signer, ply)`; it never consults chess legality, the
    chain, or arrival order. It fires at any ply, including plies the chain
    never reaches, and the walk then stops one ply short of the earliest such
    ply. Both players double-signing is a draw, as for mutual resignation.

    Position-free is not a simplification, it is the whole point. Eviction has
    to sort blind by id (legality depends on the position, which depends on the
    chain, which depends on which records are present — so a legality-aware
    rule evicts differently in a partial state and peers diverge). A
    legality-based forfeit is therefore *dissolvable*: publish a wrong-parent
    junk record and one different legal move, both lower-id than your real
    move, and eviction drops your real move while the parent check filters the
    junk. Exactly one legal candidate survives, no forfeit fires, and you have
    rewritten a ply your opponent already answered — unlimited takeback using
    the opponent as a search oracle. The same rewind revives an expired
    `DrawOffer` or `DrawClaim`, because every head you can rewind to is one
    where you were to move.

    Counting records cannot be dissolved that way: every one of these attacks
    needs two `Move` records in the attacker's own group, and eviction FLOORS
    that group at K=2 rather than emptying it. That is what makes K=2
    load-bearing rather than decorative — the junk used to hide the fraud *is*
    the fraud proof.

    Signature malleability does not trip it: two valid signatures over one body
    share an id (invariant 2), so they are one record in one slot. Two
    spellings of one castling move DO trip it — see invariant 8. Tests:
    `two_move_records_at_one_ply_forfeit_regardless_of_legality`,
    `retroactive_move_substitution_forfeits`,
    `reviving_an_expired_draw_offer_by_rewinding_forfeits`,
    `a_buried_double_sign_still_forfeits`, `a_mutual_double_sign_is_a_draw`,
    `illegal_move_is_ignored_not_fatal`,
    `signature_malleability_does_not_split_records`.

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

- **Eviction sorts blind, and that is survivable only because the forfeit is
  structural.** Eviction must sort blind by id: legality depends on the
  position, which depends on the chain, which depends on which records are
  present, so a legality-aware rule would evict different records in a partial
  state and peers would diverge.

  Blind-by-id means a player CAN evict their own real move by signing lower-id
  junk `Move` records at that ply, and can bury a *legality-based* fraud proof
  the same way. Both were, for one commit on this branch, an unconditional
  burn-the-game button and worse: leave exactly one valid candidate standing
  and the chain continues on a substituted move.

  Invariant 11 closes it. The forfeit counts `Move` records per `(signer,
  ply)`, so every version of this manoeuvre — voiding your own game,
  substituting a move, rewinding the head to revive an expired draw offer —
  requires two records in your own group and forfeits you. The fraud proof is
  RESTORED, and is strictly stronger than the original, because it no longer
  depends on chess legality and so cannot be dissolved by burial. K=2 is what
  makes it robust: eviction floors the group rather than emptying it, so an
  attacker cannot evict away their own evidence. The cost is invariant 8's
  castling case, which the stock stack cannot reach. Tests:
  `a_buried_double_sign_still_forfeits`,
  `retroactive_move_substitution_forfeits`,
  `reviving_an_expired_draw_offer_by_rewinding_forfeits`.

- **The outcome is not monotone, and eviction is the bigger source.** A
  strict superset can move the projection *down*: a decided game plus one extra
  record (a late-published double-sign fraud proof) becomes a forfeit at the
  fork point, with `fen` and `ply` rewound and the opposite winner. Peers
  holding the same set still agree, so this is not a convergence bug, and it is
  not exploitable — double-signing only ever forfeits the signer.

  Eviction widens this considerably, and is now the dominant source. Adding
  records does not merely ADD to the set; it can REMOVE from it, because a
  lower-id record displaces a higher-id one in the same `(signer, kind, ply)`
  group. So a superset can rewind the head, unseat the move the chain was
  built on, and change `fen`, `ply`, `chain`, `ignored` and the decision all at
  once — and it can do so at a ply far behind the visible head. Every such
  rewind now lands on invariant 11's forfeit rather than on a silent
  substitution, so the *direction* is safe (the rewinder loses), but the
  display still moves. The UI should show the full chain rather than the
  truncated `fen` after a forfeit.
  Tests: `superset_reverses_the_outcome_and_rewinds_the_board`,
  `retroactive_move_substitution_forfeits`.

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

## Client

`client/src/session.rs` funnels every move-flow command through `open_game`,
which resolves the bound game, checks this build's contract WASM derives the
same contract id the delegate recorded at bind time, then GETs and projects
the current state. `open_game`'s GET deliberately does **not** subscribe.
Eight commands share it — `play_move`, `resign`, `show_label`, the draw
commands, and the rest — and every one of them is a one-shot: it reads state,
maybe signs and submits a record, and returns. If `open_game` subscribed, each
of those would leave a subscription behind it that nothing ever tears down.

`watch_label` is the one flow that needs a subscription, so it asks for its
own: after calling `open_game` it issues a second, subscribing GET
(`node.get(id, true)`) before entering its update loop, and **merges that
GET's returned state** rather than discarding it. The merge is what closes the
lost-update window: between `open_game`'s non-subscribing GET and the
subscription being established, the opponent's move is broadcast to
subscribers this client is not yet among, and would be lost with no error
anywhere — a terminal on a stale position, again indistinguishable from an
idle game. `WsClient::get` additionally passes `blocking_subscribe: subscribe`
so the node establishes the subscription before answering, and queues any
`UpdateNotification` that arrives while a request loop is waiting on its own
response (`pending` in `cli/src/ws.rs`) instead of dropping it — the receive
loops each skip messages they are not waiting for, and one connection carries
both. `watch_label` also returns immediately if the game is already decided:
a finished game will never produce another notification, so entering the loop
would print the final position and hang.

`watch_label` returns `Ok(())` when `next_update` yields `None`. Against
`WsClient` that is dead code (it can never return `None`, see below); against
`FakeNode` a `continue` there is a yield-free hot loop that wedges a
current-thread runtime — which is exactly why the function went untested for
as long as it did. **This is worth
calling out because of how the omission was found, not just what the fix is.**
`watch` originally never subscribed. Against a real node it rendered the
opening position once and then blocked on `next_update` forever —
indistinguishable, from the outside, from a healthy idle game waiting on the
opponent. No test caught it, because `FakeNode` ignored the subscribe flag
entirely and handed out updates from a shared log regardless of who had asked
to watch. `FakeNode` now tracks subscriptions per node (`subscribed:
BTreeMap<[u8; 32], usize>` in `client/src/fake.rs`) and `next_update` only
yields entries for contracts that node subscribed to via `get(.., true)`, and
only those that landed **at or after** the subscribe point — the `usize` is
the log length at that moment. A subscription is not retroactive on a real
node, and a fake that replayed history to a late subscriber would hide the
lost-update window described below. So a future command that forgets to
subscribe fails a test instead of failing silently against a live node. It is the same argument the rest of this file
already makes for `FakeNode` running the real contract and delegate code: a
fake that grants a permission the real node requires is only testing the
happy path, and is worse than no fake at all.

`node::NodeClient::next_update` has no request timeout, unlike every other
method on the trait (`get`, `put`, `update`, `delegate`), which are bounded by
the 30-second `RESPONSE_TIMEOUT`. That asymmetry is deliberate: a
correspondence move can legitimately take days, so a timeout on `next_update`
would report a healthy idle game as a failure — exactly the outcome the other
methods' timeout exists to prevent for an unresponsive node. Its doc comment
also has to be read per-implementation: `Ok(None)` means "nothing waiting" for
`FakeNode`, which drains a finite in-memory log, but `WsClient` blocks on the
socket's `recv()` and has no such log to exhaust, so for a real node this call
either yields an update or does not return — it can never produce `None`.
`BrowserClient` is the exception that makes the contract real: its error
handler pushes a `Frame::Closed` on `onerror`/`onclose`, so `next_update` there
returns `Ok(None)` to mean "the socket is gone, no update will ever arrive" —
and it returns it on the *latched* close as well as on the frame, because the
frame itself is one queue item that an in-flight request may already have
eaten. See "UI" below.

`watch` has no automated test against a real node.
`client/tests/updates.rs::watch_label_reports_the_opponents_move` drives
`watch_label` end to end against two `FakeNode`s — subscribe, opponent moves,
callback fires with `ply == 1`. Be precise about what that pins down, because
the previous version of this paragraph was not. It pins: that `watch_label`
runs end to end at all; that it decodes a `Delta` payload (reverting
`FakeNode` to emit `State` fails it); and that it terminates on `Ok(None)`.

It does **not** cover the subscribing-GET merge, and mutation testing confirms
that — the test still passes with the merge deleted, with `watch_label`'s own
`node.get(.., true)` deleted, or with the `*id != g.contract` filter deleted.
The reason is structural, not an oversight in the test: both fakes share one
`World`, so `open_game`'s GET already returns the post-move state and there is
no lost-update window for the fake to simulate. Closing that gap means teaching
`FakeNode` to model per-node state divergence, which is a larger change than
the feature it would be testing. The CLI's argument parsing and rendering for
`adjourn watch` is likewise not exercised by any test.

`FakeNode` broadcasts the **delta** for an update and whole **state** for a
PUT (`Broadcast` in `client/src/fake.rs`), matching what a real node sends
subscribers: `sign_and_submit` submits a delta, so `UpdateData::Delta` is the
arm `watch` actually runs in production. A fake that only ever emitted `State`
left the live arm untested. All three arms now *report* a decode failure
instead of swallowing it (`decode_state_payload` / `decode_delta_payload` in
`session.rs`) — a dropped decode error is a board that silently never updates,
which is the same failure signature this section has already described twice.

**An empty payload is not a decode failure**, and both helpers return
`Ok(None)` for one rather than erroring. This is the same rule as "empty state
is valid, not malformed" above, and it is load-bearing in both directions: a
contract is PUT with `Vec::new()` before either player moves and that PUT is
broadcast verbatim, while `get_state_delta` deliberately emits ZERO bytes for
an empty delta so freenet-core's "empty delta -> skip broadcast" path can fire
(the `self_delta_empty` fix). `GameState::decode` returns `None` on zero bytes,
so erroring on a decode failure without splitting the empty case out first
exits `adjourn watch` non-zero mid-game over a perfectly legal broadcast. The
subscribing GET runs through the same two helpers so the rule cannot drift
between the two paths.

`adjourn-client` takes randomness as a **parameter** (`invite_new`'s `entropy`
and `nonce`, `invite_accept`'s `entropy`) rather than generating it. The crate
exists to be reachable from a browser, and `rand` -> `rand_core` ->
`getrandom` is a hard compile error on `wasm32-unknown-unknown` — the same
dependency banned from the contract and delegate graphs, which a
workspace-wide feature unification would drag in behind this crate. The CLI
supplies the bytes from `rand`; a browser will supply
`crypto.getRandomValues`. Hoisting where the bytes come from does not change
who *authors* them: the `GameParams` nonce still has exactly one author, the
inviter. CI asserts the property directly with `cargo check -p adjourn-client
--no-default-features --target wasm32-unknown-unknown`; a native
`--no-default-features` check cannot catch it.

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
  `DelegateCtx`, so `client/src/fake.rs`'s `FakeNode` runs the real handler logic
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

## UI

`ui/` builds `adjourn-ui`, a Dioxus web frontend that compiles to
`wasm32-unknown-unknown` and runs in a browser tab. It sits in the same
workspace as the contract and delegate crates without contaminating either:
`scripts/build-contract.sh` and `scripts/build-delegate.sh` build with `-p`,
package-scoped, so no workspace-wide build ever unifies the UI's dependency
graph into the contract's or the delegate's. River does the same — a UI crate
and a contract crate can share a workspace as long as nothing ever builds them
together.

**Two `freenet-stdlib` feature facts that look contradictory, and are not.**
`adjourn-client` depends on `freenet-stdlib` **without** `net` (see the
workspace `Cargo.toml`), because that crate is shared with the CLI, and `net`
pulls `tokio-tungstenite` on native targets — dead weight for a CLI that has
its own tungstenite transport in `cli/src/ws.rs`, and dead weight this crate
cannot afford at all once it has to compile for wasm. `adjourn-ui` depends on
`freenet-stdlib` **with** `net` (see `ui/Cargo.toml`), because on
`wasm32-unknown-unknown` that is exactly what provides the browser `WebApi` —
upstream gates `tokio`/`tokio-tungstenite` to `cfg(any(unix, windows))` and
`web-sys`/`wasm-bindgen` to `cfg(target_family = "wasm")`, so the identical
feature flag resolves to different code, and pulls in different dependencies,
depending on which target is compiling. Turning `net` off in `ui/Cargo.toml`
"to match the other crate" would delete the browser transport entirely; adding
it to `adjourn-client`'s base dependency "for consistency" would put
`tokio-tungstenite` back in the CLI's native build for no reason. Both facts
have to hold at once, on purpose.

**`default-features = false` is silently ignored on a workspace-inherited
dependency, in cargo 1.97.1.** The obvious way to write `ui/Cargo.toml` is
`adjourn-client = { workspace = true, default-features = false }`. That
compiles, and cargo does not error — it warns that overriding
`default-features` on a `workspace = true` dependency "could become a hard
error in the future," then resolves with the workspace's defaults anyway. The
workspace default for `adjourn-client` is `fake` **on** (so `cargo test
--workspace` works with no extra flags), and `fake` pulls in
`adjourn-contract` and `adjourn-delegate` — exactly the crates and WASM
toolchains this crate exists to keep out of the browser bundle. This was
caught with `cargo tree -p adjourn-ui --target wasm32-unknown-unknown -e
normal`, not assumed: the contract and delegate showed up in the graph until
`ui/Cargo.toml` was changed to a direct `path` dependency
(`adjourn-client = { path = "../client", default-features = false }`), which
cargo does honor. `-e normal` is not decoration, and it earns its place the
opposite way round from the obvious guess — this was measured. `-e normal`
prints only the graph that actually **ships**, so it is immune to
dev-dependencies: restoring an `adjourn-client` dev-dep gives **0** hits,
because the dev edge is dropped outright. The *plain* `cargo tree` is the
misleading one — a dev-dep resolves with default features, turning `fake` back
on, so it prints the contract and the delegate even when the shipping graph is
clean. What `-e normal` does catch is the regression that matters: reverting
`ui/Cargo.toml` to `workspace = true, default-features = false` puts both
crates in the graph (measured, 2 hits), and the path form clears it. There is
deliberately no `adjourn-client` dev-dep anyway (an integration test already
sees the package's normal dependencies), and CI's "Assert the UI graph excludes
the contract and the delegate" step runs this same command so a revert cannot
pass unnoticed. Every other crate in this workspace depends on its siblings
via `workspace = true`; `ui/Cargo.toml`'s deviation looks like an
inconsistency an editor should "fix." It is the one dependency in the
workspace where `workspace = true` silently does the wrong thing, and it is
the direct-path form that is correct.

**The UI needs no `getrandom`, for the same reason `adjourn-client` takes
entropy as a parameter (see "Client" below).** A browser has no
`rand`/`rand_core` in its dependency graph to begin with — `ui/src/node.rs`'s
`browser_entropy` draws 32 bytes straight from `crypto.getRandomValues` via
`web_sys`, and hands them to `adjourn-client`'s entropy parameters exactly as
the CLI hands it bytes from `rand`. That hoist is what keeps this crate's
wasm build free of `getrandom`'s wasm-bindgen placeholder imports — the same
import-resolution failure that would otherwise stop the *contract* from
instantiating (see "No `rand` or `getrandom`, ever" above) would just as
readily stop this crate from compiling for wasm at all.

**`adjourn-ui` has to compile for two targets, and each direction caught a
real defect it would otherwise have hidden.** `cargo check --target
wasm32-unknown-unknown` is the shipping check — that is the only target this
crate is ever actually deployed to. But `cargo check --all-targets` on the
host is what lets `cargo test --workspace` run at all, and for a while in this
work the crate compiled cleanly for wasm while failing to compile natively:
`freenet-stdlib`'s `WebApi` is gated `cfg(all(target_family = "wasm", feature
= "net"))` to the callback-based browser type, and resolves to a different,
single-argument, non-callback type on a native target. A `BrowserClient` that
used the wasm shape unconditionally would not compile natively at all — which
would silently take the native-only board tests down with it (no test runner
can build the test binary) and leave the `route`/`Routed` unit tests dead
code nobody was running. That is why `BrowserClient` and `browser_entropy` in
`ui/src/node.rs` are gated `cfg(target_arch = "wasm32")`, while `route` and
`Routed` are deliberately left target-independent: `HostResponse`,
`ContractResponse` and `UpdateData` exist on every target, so pulling the pure
classification logic out from under the gate is what lets it run — and be
tested — on the host at all.

**`connect` resolves on failure, not only on success.** `freenet-stdlib`'s
browser `WebApi::start` wires both `onerror` and `onclose` to the same
error-handler closure and never to the `onopen` callback. A `connect` future
that only awaited `onopen` would hang forever, with no error and no timeout,
against a node that refuses the connection, a bad URL, or a node that is
simply down — indistinguishable from the outside from a client that is still
trying. `BrowserClient::connect` in `ui/src/node.rs` instead races a single
take-once `oneshot` sender across all three callbacks: whichever fires first
resolves the future, and a later callback firing on an already-taken sender
is a harmless no-op rather than a panic on a repeat send.

**And `connect` is now bounded by `RESPONSE_TIMEOUT` too, closing a second,
narrower hang the paragraph above does not.** Resolving on `onerror`/`onclose`
closes the *refused*-connection case — the port is closed, the OS resets the
connection, and `onclose` fires promptly. It does nothing for the SYN that is
silently *dropped* rather than refused — a firewall, a VPN, a sandboxed CI
network — where no `onerror`, no `onclose`, and no `onopen` ever fire, so
nothing resolves the oneshot: the original hang, with a narrower trigger, and
indistinguishable from the outside from a slow node. `connect` now races the
same 30-second `RESPONSE_TIMEOUT` every request already uses; `next_update`
remains the one deliberate exemption, since a correspondence move can take
days but a TCP handshake cannot. Loopback refuses promptly, so the dead-port
browser test below was passing on an assumption this fix makes explicit
rather than incidental — before it, that test would have hung until the
harness killed it on a drop-instead-of-refuse network, not failed cleanly.

**The error handler stays live for the client's lifetime, and every failure
becomes a `Frame`.** The take-once sender above solves `connect` and nothing
else: an error handler that goes quiet once connect resolves swallows every
later `onerror` and `onclose`, which is every way a socket can die. And the
inbox cannot signal the death by ending, because `freenet-stdlib` `forget()`s
its onmessage closure (`client_api/browser.rs` ~125) — the sender is leaked,
so `inbox.next()` never returns `None` and any "connection closed" arm written
against it is unreachable dead code. So the handler does two jobs: resolve
`connect` at most once, and *always* push a frame into the same inbox the
responses arrive on.

**But not every call of that handler is a dead socket, and the ones that are
must be latched.** Two corrections to the paragraph above, each of which was a
live bug in the first version of it.

`freenet-stdlib` funnels far more than socket death through the single error
handler: a non-binary frame, a bincode deserialize failure, two stream
reassembly failures, and four send-side paths all call it, alongside the
genuine `onerror` and `onclose` (`browser.rs` ~55, ~69, ~100, ~112, ~187,
~204, ~238, ~242 versus ~128 and ~152). Synthesising `Closed` for all of them
means **one undeserialisable frame ends `watch` on a live socket, silently** —
precisely what `watch` exists to prevent. `socket_is_gone` draws the line on
the `source` tag the stdlib puts in its JSON payload: only `"close"` and
`"exec error"` are the socket dying; everything else becomes a `Frame::Failed`,
which errors a waiting request and is skipped by `next_update`. The tag was
chosen over `WebSocket::ready_state()` because it is pure data, so the
decision is unit-tested off-wasm rather than being browser-only — and
`ready_state` is not even reliable at `onerror` time.

And a graceful close fires `onclose` **once**. One frame, one waiter: a
request in flight consumes it and bails correctly, the app resumes watching,
and `next_update` — which has no timeout by design and reads an inbox that can
never end — parks forever on a dead socket, showing a stale board. So
`CloseLatch` records the first genuine close and both `next_response` and
`next_update` check it *before* awaiting. A survivable error must never latch
it, or one bad frame permanently convinces the client the node is gone.

That inbox carries `Frame`, not `HostResponse`, and that is the second half.
`WebApi`'s result handler takes `Result<HostResponse, ClientError>` — the node
reports per-request failures (a rejected `Update`, a contract execution error,
an `ApplicationMessages` against an unbound key) through the `Err` side. An
`if let Ok(resp)` there drops all of them, and the waiting request never wakes:
a rejected move spins forever, looking exactly like a healthy idle
correspondence game. `route` therefore classifies four cases, not two —
`Response`, `Notification`, `Failed`, `Closed` — and it is ungated and pure, so
all four are unit-tested off-wasm, as are `socket_is_gone` and `CloseLatch`.

**`BrowserClient` bounds a request with `setTimeout`, mirroring
`cli/src/ws.rs`.** Same 30-second `RESPONSE_TIMEOUT`, same "name the operation
that hung" error style, and the same deliberate exemption for `next_update` —
see the asymmetry documented under "Client" above. There is no `tokio::time`
on this target, so the timer is a `web_sys` `setTimeout` wrapped as a future;
that is the only reason `wasm-bindgen` is a direct dependency of this crate.
The `Closure` is held by the future and the handle is cleared on drop, so a
request that answers in time leaves no pending callback and no leak.

**A live node answers a GET for a contract it has never seen with `Err`, not
`Ok(None)` — and every transport, including this one, has to fold that one
case back to `Ok(None)` itself.** `NodeClient::get`'s contract is "`Ok(None)`
means the network does not have this contract yet." Against a real `freenet
0.2.130` node, `game_bind`'s inviter path (GET, then PUT only if absent) hit
`Err(ContractError::MissingContract)` instead, so the `?` propagated and the
PUT never ran — against a real node the invite exchange could never complete.
`BrowserClient::get` in `ui/src/node.rs` now has an explicit
`Routed::ContractMissing` arm (alongside `cli/src/ws.rs`'s `WsClient::get`)
that returns `Ok(None)` for exactly this case, classified on the typed
`ErrorKind::RequestError(ContractError::MissingContract)` the node reports —
never on the rendered error string, since a reworded upstream message must
not silently stop matching, and folding some *other* failure into "absent"
the same way would let a caller silently PUT over a live game's contract.
`WsClient::get`'s own wiring of this classifier has no test — `cli/` has no
`tests/` directory at all — so it is verified by reading only; the classifier
itself (`is_missing_contract` in `client/src/node.rs`) has unit tests, and
`ui/tests/routing.rs` covers the `ContractMissing` routing on this crate's
side. No test through `FakeNode` could have caught the original bug: every
existing test ran both players against one shared in-memory `World`, so the
accepter's unconditional PUT had already populated the contract by the time
the inviter's conditional GET looked, and the absent branch never ran at all.

**An `include_bytes!` into a `const` costs zero bytes in the shipped wasm
until something actually reads the bytes at runtime — and `.len()` does not
count, because it const-folds.** `CONTRACT_WASM`/`DELEGATE_WASM` in
`ui/src/lib.rs` are each an `include_bytes!`; the bring-up measured that the
built app wasm was byte-identical with and without a length read of either
constant, and grew by exactly 1,376,063 bytes — 267,003 for the contract plus
1,101,953 for the delegate — only once code that runtime-folds the data (an
actual use, such as passing the slice to `delegate_container`) was reached.
Anyone auditing whether the two modules actually ship in a build has to test
for the byte growth, not for the presence of the constant in source — the
constant is in source either way, dead or not, until something forces it in.

**`dx` appends `[web.app] title` from `Dioxus.toml` into whatever `<title>`
it finds in `index.html`, and does nothing when there is none.** Text
already in the `<title>` element gets the configured title prefixed onto it
("adjournadjourn" for `title = "adjourn"` and `<title>adjourn</title>`);
deleting the `<title>` element entirely means `dx` has nothing to inject into
and the page ships with no title at all. An empty `<title></title>` paired
with the value in `Dioxus.toml` is the only combination of the three that
produces exactly the configured title — `ui/index.html` and `ui/Dioxus.toml`
both carry comments recording this so it does not get "cleaned up" back to
one of the two broken forms.

## The app that now exists

`ui/src/app.rs` is the shell: `Screen` (`List`, `New`, `Accept`, `Game(label)`,
`Settings`) picks which of `ui/src/views/{list,setup,game,settings}.rs` renders,
and it is the one place the error banner and the busy spinner are drawn, so
every screen shares one failure surface rather than each rolling its own. It
mounts with one `Cmd::ListGames`, which doubles as the browser's `adjourn
init`: the actor registers the delegate as part of connecting, before the
first command it actually needs to answer.

**One coroutine owns the client, and that is forced, not stylistic.**
`BrowserClient` takes `&mut self` on every method and is not `Clone` — the
obvious `Rc<RefCell<BrowserClient>>` shared through a Dioxus context panics at
runtime, because every method call awaits and a `RefCell` borrow cannot be
held across an `.await`. `ui/src/conn.rs`'s `use_conn` instead spawns a
coroutine that owns the client outright and serialises every `Cmd` through
one channel — screens never touch the transport, they send a `Cmd` and read a
`Signal`. That removes the hazard structurally instead of managing it with
runtime borrow discipline that would panic the first time two commands
overlapped.

**A second, dedicated coroutine exists for exactly one command, `Watch`, and
for a reason spelled out in `conn.rs`'s own comments.** `watch_label` does not
return until the game ends — there is no timeout, by design, because a
correspondence move can take days — so running it on the main actor would
block `Resign`, `Play`, `ListGames`, and everything else for the rest of the
game. The watch coroutine holds its own second `BrowserClient` to the same
node (two sockets to one local node is cheap, and each still has exactly one
owner, the rule that makes `BrowserClient` safe to hold across an `.await` at
all) and races the in-flight `watch_label` future against `rx.next()` via
`futures::select!`. Two things about that race are easy to get backwards
later:

- **The losing future is *dropped*, and dropping cancels it — no
  cancellation hook is needed in `session::watch_label`.** A "cleaner"
  refactor that gave the watch an explicit cancel signal would be solving a
  problem `select!` + `Drop` already solves for free.
- **Without the race, only the first game ever opened gets live updates.**
  A version that ran `watch_label` to completion before accepting its next
  command would never see the command that opens a second game while the
  first is still being watched — that command would simply queue behind a
  future that does not resolve until the first game ends. The race is what
  lets opening game B while game A is being watched win immediately, cancel
  A's watch, and re-issue a fresh `Watch` for A only when the UI asks again.

**`Cmd::Open` clears `view` before awaiting, and `GameScreen` filters on
label — two independent guards against the same failure, deliberately.**
`view` is one `Signal<Option<GameView>>` shared by every game screen. If
`Cmd::Open { label }` left the previous game's view in place while its own
GET was in flight, that stale board would render under the *new* label for
the whole request — and forever, if the open then errors, since nothing else
would ever clear it. So `conn.rs` sets `view.set(None)` before awaiting the
open. Independently, `ui/src/views/game.rs`'s `GameScreen` reads `wires.view`
filtered by `v.label == label`, so even a `view` update that lands for the
*wrong* screen (a race between switching screens and an in-flight command
resolving) renders nothing rather than the wrong game's board under the right
game's chrome. Removing either guard on the assumption the other one covers
it would reopen exactly the failure mode the other was written to close.

## Coverage — what runs, what a browser is needed for, and what "browser
tests exist" does and does not close

`board.rs` has 8 tests, all run natively, all pure-function tests with no
network and no DOM. `node.rs`'s `route` and its neighbours have 14 tests
(`ui/tests/routing.rs`), also native, also pure — including the failure
classifications (`socket_is_gone`, `CloseLatch`, and the `ContractMissing`
routing added alongside the missing-contract fix below), pulled out from
under the wasm gate precisely so they could be tested without a browser.

**`ui/tests/browser.rs` exists now, and it is real coverage — but it is not
CI coverage, and CLAUDE.md previously said this crate had never been loaded
in a browser at all. That is no longer true.** The app has been built with
`dx` 0.7.9 (`dx build --platform web`, `dx serve`), served, and driven both by
hand and by Playwright against a live `freenet local` node: connecting,
listing games, opening a game, and playing a move all render correctly and
advance the projected board. Two `#[wasm_bindgen_test]` cases in
`ui/tests/browser.rs`, run in headless Firefox against a live node on 7509,
cover `connect`'s dead-port failure path and a live round trip
(`register_delegate` then `ListGames` through the real delegate). They are
`#![cfg(target_arch = "wasm32")]`, so a native `cargo test --workspace`
compiles the file to nothing and runs zero tests from it — confirmed in the
run below (`Running tests/browser.rs` / `running 0 tests`). They also have
**no skip path**: unlike the `client/tests` that skip themselves when the
contract WASM is absent locally, a missing node here is a hard failure, not a
silent pass, so this file can never masquerade as green when nothing was
actually checked.

**They are NOT wired into CI.** CI has no browser and no `geckodriver`, and
nothing in this branch added either. Running them requires `wasm-pack test
--headless --firefox ui` (or an equivalent `wasm-bindgen-test` runner) plus a
`freenet local` node reachable at the URL the test hardcodes — set that up
manually, e.g. in a future CI job with a browser and a node fixture, before
these can run unattended. Until then they are a local, on-demand check.

**What still has no automated coverage at all, even with these two tests
landed:** the `setTimeout` that bounds a request (`RESPONSE_TIMEOUT` on
`get`/`put`/`update`/`delegate`), and that the error handler genuinely keeps
firing *after* `connect` resolves rather than going quiet — both would need a
node that answers slowly or errors mid-session, which neither of the two
browser tests exercises. Those two remain verified only by source-reading and
by the fact that `connect`'s own bound (see above) is now covered by the
dead-port test.

The workspace test count below is **168** and does not include the 2 browser
tests — they run under `wasm-pack`/`wasm-bindgen-test`, never under `cargo
test`, so they cannot appear in that number by construction, not by
omission.

## Testing

`cargo test --workspace --locked` — 168 tests: 99 in `adjourn-core` (24 algebra
tests, 35 adversarial tests, and 40 delegate-policy tests), 17 contract tests,
9 delegate adapter tests, 21 `adjourn-client` tests, and 22 `adjourn-ui` tests
(8 board, 14 routing — see "UI" above for what that number does and does not
cover). Two more `adjourn-ui` tests exist in `ui/tests/browser.rs` and are
**not** in the 168 — they compile to nothing under a native `cargo test`
(`#![cfg(target_arch = "wasm32")]`) and only run under `wasm-pack test
--headless --firefox`, against a live node; see "UI" above. The algebra tests
are the point; they run randomized partitions and delivery orders. Keep them
green. New state-shape features need a corresponding law test, not just a
happy-path test.

- `common/tests/algebra.rs` (24) — the monoid laws and the original
  adversarial cases.
- `common/tests/adversarial.rs` (35) — convergence attacks, outcome
  precedence, the structural double-sign forfeit and the two attacks it closes
  (retroactive move substitution, reviving an expired draw offer by rewinding),
  and the chess edges (promotion, underpromotion, castling notation, en
  passant, repetition, top-K eviction, `MAX_PLY`, and draw claims).
- `common/tests/delegate_policy.rs` (40) — the delegate's pure decision
  functions: bind/sign refusals, entropy classification and mixing, the
  ply-0 sentinel guard, and wire round-trips for `Request`/`Response`/
  `GameRecord`/`GameSummary` through CBOR. Runs on any platform.
- `contracts/adjourn-contract/tests/interface.rs` (17) — the adapter: byte
  encodings, empty-state cases, two peers converging in one round through the
  real interface, and that chess legality is NOT a validity condition.
- `delegates/adjourn-delegate/tests/adapter.rs` (9) — CI-only. Two groups:
  the secret-store key namespaces never collide (and a crafted label cannot
  forge another namespace's prefix), plus dispatch tests that drive
  `adjourn_delegate::handle` directly — a key created and listed, a label
  hijack attempt from a different origin refused as `WrongOrigin`, and a
  double-sign attempt refused through the real dispatch path, not just the
  policy layer beneath it.
- `client/` (21, across `src/lib.rs` unit tests 2, `tests/fake_node.rs` 2,
  `tests/full_game.rs` 1, `tests/invite.rs` 4, `tests/moves.rs` 4,
  `tests/setup.rs` 3, `tests/updates.rs` 3, `tests/view.rs` 2) —
  `adjourn_client::session`'s flows run against `FakeNode` (real contract and
  delegate code, in-memory transport): both players deriving the same
  contract, a build mismatch refused loudly, a full scholar's-mate game end
  to end, out-of-turn moves failing before signing, a double-sign attempt
  refused by the delegate, (`updates.rs`) `watch_label` driven end to end
  against `FakeNode`'s per-node subscription tracking asserting the
  notification is a `Delta` and the watcher's callback reports the
  opponent's move (see "Client" above), (`setup.rs`) each side of an invite
  getting its own `FakeNode` `World` rather than sharing one — the structure
  that lets the inviter's conditional GET actually hit the absent-contract
  branch, which is what the missing-contract fix needed (see "UI" above) —
  and (`view.rs`) `GameView`'s ordered move list. These flow tests moved here
  from `cli/tests/` when the session logic was extracted into
  `adjourn-client`; the CLI crate itself has no `tests/` directory at all —
  which is also why `WsClient::get`'s wiring of the missing-contract
  classifier is verified by reading only, not by a test. Several integration
  tests read the compiled contract WASM off disk and skip themselves if it is
  absent locally -- but panic instead if `CI` is set, so a skip can never
  masquerade as a pass in CI (see `client/tests/common/mod.rs::contract_wasm`).
- `ui/tests/board.rs` (8) and `ui/tests/routing.rs` (14) — both run natively,
  not on wasm: the opening position and its mirror for Black, square
  selection and legal-move highlighting, promotion detection on both back
  ranks, and (`routing.rs`) that `route` tells a response from an update
  notification and never confuses the two, that a node-reported `Err` frame
  routes to `Failed` rather than being dropped, that a socket close routes to
  `Closed`, and that a `MissingContract` node error routes to
  `Routed::ContractMissing` and is never mistaken for an ordinary `Failed`
  (the missing-contract fix's routing, distinct from the other three failure
  kinds). Then the two halves of the close story: `socket_is_gone` accepts
  only `onclose`/`onerror` and refuses every decode and reassembly tag (a
  live-socket error reported as a close would silently end `watch`), and
  `CloseLatch` keeps the first close reason forever while no survivable error
  ever latches it (a close is one queue item, so a request in flight can
  consume the only one and leave `next_update` parked on a dead socket with no
  timeout to save it). All of these are the transport's error decisions pulled
  out as pure functions precisely so they are testable off-wasm — the same
  move `route` itself was extracted for. `ui/tests/browser.rs` (2, wasm-only,
  **not** counted in the 168) covers `connect`'s dead-port timeout and a live
  `register_delegate`/`ListGames` round trip — see "UI" above for exactly what
  that file does and does not close, and why it is not wired into CI. What
  still has no automated coverage anywhere: the `setTimeout` that bounds a
  request, and that the error handler keeps firing after `connect` resolves.

### Check against the network's own verifier

`fdev conformance` runs the *same* verifier freenet-core runs, over a corpus of
real states, and it catches things our own tests structurally cannot:

The corpus generator is committed, because the corpus has to be rebuilt on
every wire-format change and the previous one lived only in a throwaway
working copy — which is why the pre-`algebra` result could not be reproduced.

```sh
cargo run -p adjourn-core --example dump_corpus -- corpus/
fdev conformance   --wasm target/wasm32-unknown-unknown/release/adjourn_contract.wasm   --params corpus/params.bin   $(for s in corpus/state_*.bin; do printf -- "--state %s " "$s"; done)   --max-cases 20000
```

`--max-cases` matters: the default is 512, which this corpus exceeds, so the
default silently *samples* instead of exhausting. At 20000 the run terminates
on its own at 817 cases — that number is the corpus fully explored, not a cap.

It found `self_delta_empty` — `get_state_delta` returned a CBOR-encoded empty
list (one byte, `0x80`) instead of zero bytes, so freenet-core's "empty delta ->
skip" broadcast path could never fire. Our tests missed it because they assert
on the DECODED delta, which really is empty; the network reads the encoded
length. That is the same failure that drove River's 2026-07-25 incident, where
the room contract took 63.7% of network-wide byte-weighted broadcast work.

All three read paths — `update_state` on its base state, `summarize_state`,
and `get_state_delta` — call `filter_valid(&params)` before merging,
summarizing or diffing, for the same never-settles reason. Making them
identical is the point: the same stored bytes must produce the same record set
whichever entry point reads them.
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
only bite when peers hold different fragments.

**Status: 19 states, 817 cases, 817 held, 0 violations** — against the
post-`algebra` wire format, `fdev` 0.3.292 / freenet 0.2.130, 2026-08-25.
Contract `Gnp1PdFr2chgGpzEhKqZ8Cd4pEXvcqV1FPC49VESrsyE`, code hash
`D8xBvq5UCFSPxQXogtynvXLwDXmD55C7Tuq9NsGM3Bfm`. The corpus is weighted toward
partial and adversarial states — overlapping fragments of one game, a crafted
over-K group, the double-sign and substitution shapes, stale and live draw
records, a claim, a record at `MAX_PLY`, and the two-valid-signatures
collision — because merge only has work to do when peers hold different
fragments.

The generator refuses to emit two byte-identical states. That is not tidiness:
`fdev` deduplicates silently, and signing is deterministic, so a one-move
`e2e4` state built through `make_move` is the same bytes as one signed by hand.
The first run of this corpus reported 18 states for 19 files and said nothing
about the difference.

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
- **A GET for a contract a node has never seen answers `Err`, not
  `Ok(None)`.** Confirmed against a live `freenet 0.2.130` node while binding
  through the UI: `game_bind`'s inviter path (GET, PUT only if absent) hit
  `Err(ContractError::MissingContract)` and the PUT never ran, so the invite
  exchange could not complete. Fixed at the transport boundary in both
  `WsClient::get` and `BrowserClient::get` — see "UI" above and the "Client"
  section for the full account, including why no `FakeNode` test caught it.
- **Two `freenet local` nodes on one machine do NOT peer, and the two-node
  live-update path remains unverified end to end because of that, not because
  of a UI defect.** Both nodes in `docs/runbook-two-nodes.md`'s setup resolve
  `mode = "local"` in their generated config — the binary's own `--help`
  calls this "local-only mode... no real P2P" — so ports 7509 and 7510 never
  peer, and one node's contract storage is fully isolated from the other's
  regardless of how many times the same deterministic contract is PUT to
  each. Driving the UI against this pair confirmed the gap is environmental,
  not a bug in `conn.rs` or `watch_label`: playing `e2e4` on one node's board
  advanced that node's own ply from 0 to 1 correctly, while the other node's
  board stayed at ply 0 — and forcing a fresh `Cmd::Open` on the receiving
  side (bypassing the watch coroutine entirely, a plain re-GET) returned the
  same un-advanced state from that node's own storage. The move never left
  the first node at all; nothing was there for a subscription to miss. Actual
  peering needs `freenet network --is-gateway` on one side and `freenet
  network --gateway "host:port,pubkey"` on the other, which
  `docs/runbook-two-nodes.md` does not set up as written — the runbook's "the
  other side sees your move" checks in section 4.5 are therefore unverified
  against this exact setup, and the file has been annotated accordingly. This
  is exactly the same class of gap `CLAUDE.md` already records for `watch`
  against `FakeNode` rather than a real node: the mechanism is exercised, the
  live cross-peer path is not.

## Roadmap

1. `ContractInterface` wrapper (`validate_state` → `all_valid`, `update_state`
   → `merge`, plus `summarize_state` / `get_state_delta`)
2. Delegate holding the per-game signing key; UI never sees it
3. UI over the WebSocket API: `get`, `subscribe`, `update`, `watch` — all
   done. `watch` is backed by `NodeClient::next_update`, which is
   deliberately unbounded by the 30-second `RESPONSE_TIMEOUT` every other
   `NodeClient` method carries: a correspondence move can legitimately take
   days, and a timeout would report a healthy idle game as a failure. See
   "Client" above for the subscription requirement this uncovered and for the
   corresponding gap — `watch` is covered against `FakeNode`, not against a
   real node.
4. `ui/` (`adjourn-ui`): a Dioxus web frontend implementing that transport in
   a browser (`BrowserClient` in `ui/src/node.rs`) plus a pure board
   projection (`ui/src/board.rs`), a connection actor (`conn.rs`), and the
   four screens (`app.rs`/`views/`) — done, and it has now actually been
   built with `dx` 0.7.9, served, and driven against a live `freenet local`
   node: connecting, listing games, opening a game, and playing a move all
   work. `ui/tests/browser.rs` adds two `wasm-bindgen-test` cases against a
   live node, but they are not wired into CI (no browser, no `geckodriver`
   there). What remains unverified end to end is the cross-peer live-update
   path — see "Runtime assumptions, verified" above for why the two-node
   setup this needs does not currently peer. See "UI" above for exactly what
   is and is not tested.
