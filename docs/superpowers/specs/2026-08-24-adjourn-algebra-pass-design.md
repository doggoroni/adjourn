# adjourn: the bounded-state algebra pass

**Status:** approved, not yet implemented
**Date:** 2026-08-24

## Why now

The contract key is `hash(code, params)`, and `adjourn-core` compiles into the
contract WASM. Every change in this document rotates that key and invalidates
every signature and every record id. No games exist on the network yet, so this
is the cheapest such change will ever be. After this pass the wire format is
treated as frozen.

Four known issues close here, and they turn out to be one mechanism plus two
small independent fixes:

1. Unbounded state growth (eviction + a structural ply cap)
2. Threefold and fifty-move reported but not claimable (`DrawClaim`)
3. `ply: u16` overflow with no explicit cap (subsumed by the ply cap)
4. `Status.ignored` imprecision

## What this costs

> **AMENDED DURING IMPLEMENTATION — the analysis in this section was
> superseded. It is kept as the record of what was designed and approved at the
> time; do not read it as the shipped behaviour.**
>
> Two things below turned out to be false. The forfeit's evasion does not stop
> at a stalled chain: an attacker who leaves exactly one *valid* candidate
> standing rather than zero — a wrong-parent junk record plus a different legal
> move, both lower-id than the real one — makes `walk` continue on the
> substituted move instead of stalling. That is retroactive move substitution,
> an unlimited takeback using the opponent as a search oracle. The same rewind
> of the head revives expired `DrawOffer` and `DrawClaim` records. So "no win
> is ever stolen" was wrong.
>
> The human partner ruled for a **structural, position-free double-sign
> forfeit**: a signer holding two or more `Move` records at one ply forfeits,
> counted without consulting legality. This restores the fraud proof in a
> stronger form and makes K=2 load-bearing. Invariant 8 was reopened as the
> accepted price. See `CLAUDE.md` invariant 11 for the rule as shipped.

One guarantee gets weaker, deliberately, and this is the single most important
line in the document.

**Eviction makes the double-sign forfeit evadable.** `walk` forfeits a player
holding two distinct *legal* moves at one ply. Legality is a function of the
position, which is a function of the chain, which is a function of which records
are present — so eviction cannot consider it, and must sort blind by id. A
cheater who double-signs can therefore bury both real moves under lower-id
*illegal* move records at the same ply. `walk` then finds zero legal candidates,
the chain stops one ply short, and no forfeit is detected. Any fixed K is
beatable this way.

The trade was accepted on these grounds:

- The cheater gains nothing new. The outcome is a stalled game with no decision
  — exactly what walking away already produces, which is an open issue with the
  standing answer "let it die."
- No win is ever stolen. It converts a loss into a no-result.

`CLAUDE.md`'s current claim that "K=2 preserves the double-sign fraud proof" is
false under this design and must be rewritten, not merely softened. A test
encodes the degradation so it cannot silently regress into something worse.

A witness-published fraud-proof record that embeds both offending records was
considered and rejected for scope: defining fraud without a position collides
with invariant 8, since `e1g1` and `e1h1` are two bodies spelling one castling
move and telling them apart requires the position.

## 1. Eviction

Merge becomes `A ⊕ B = topK(A ∪ B)`, keeping the K smallest record ids within
each group.

The law that keeps the monoid intact is that top-K distributes over union:

```
topK(topK(A) ∪ topK(B)) = topK(A ∪ B)
```

The K smallest ids of `A ∪ B` are necessarily present in `topK(A) ∪ topK(B)`, so
filtering distributes, and associativity, commutativity and idempotence all
survive unchanged.

### Group key

The group key is `(signer, kind, ply)`.

`kind` is load-bearing, not tidiness. Were groups keyed on `(signer, ply)` alone,
a player could flood `DrawOffer` records at ply *N* and evict their own **moves**
at ply *N* — including both halves of a double-sign fraud proof. Separating by
kind means a move only ever competes with other moves.

Grouping *per signer* is what makes eviction safe against an opponent: your
records only ever compete with your own, so no opponent can evict your move, and
a player who spams themselves out of a legal move merely stalls their own game.

| kind | K | rationale |
|---|---|---|
| `Move` | 2 | keeps both fraud-proof slots (degraded as described above) |
| `DrawOffer` | 1 | at a given ply there is exactly one head, so exactly one legitimate `at` |
| `DrawAccept` | 1 | same |
| `DrawClaim` | 1 | same |
| `Resign` | n/a | unit variant: one possible body, so one possible id per signer |

K=1 on the draw kinds costs an honest player nothing, because an honest player
has exactly one legitimate record to make per `(kind, ply)`. It costs a
self-spamming player their own live draw offer. That is the intended asymmetry.

### Where it runs

Eviction runs **inside `merge`**, as a single pass after the union rather than
per-insert — one `O(n)` pass rather than `O(n²)`.

- `filter_valid` merges into an empty state, so it evicts. Every write path is
  therefore covered.
- `insert_verified` evicts only the affected group.
- `all_valid` (the contract's `validate_state`) does **not** reject an over-K
  state. Rejecting would be a content judgment of exactly the kind invariant 1
  forbids, and any peer that merges the state normalizes it anyway. The
  consequence — a peer may store a non-canonical state until something merges it
  — is accepted and documented.
- `absorb_for_test` continues to bypass both verification and eviction, so tests
  can build the states an attacker would put on the wire.

### Sync properties

Whitepaper Property 1 survives. `δ = A.delta_against(B.summary)` excludes only
records B already holds byte-identically, and those are in B, so `B ∪ δ = B ∪ A`
and `topK` of both sides agree.

A transient does exist: if B holds a record that A evicts, A absorbs and drops
it, so A's next summary still lacks it and B offers it again. This resolves as
soon as B merges A's records in the other direction, which normal bidirectional
sync does. A test covers it.

## 2. MAX_PLY

```rust
pub const MAX_PLY: u16 = 4096;
```

Enforced **structurally**, in `Record::verify`: any record carrying a `ply`
greater than `MAX_PLY` is not a valid record. This is a pure per-record
predicate, so it distributes over merge trivially and cannot introduce the
partial-state divergence that a chain-length-dependent rule would.

This is what bounds the *number* of groups, and therefore the state:

- 5 records per signer per ply (2 moves + 1 each of three draw kinds)
- 10 per ply across both signers
- ~41,000 records, ~6.4 MB, at ~157 bytes per record

For calibration, the longest recorded competitive game is 269 moves (538 plies);
4096 plies is 2048 full moves. A game long enough to be rejected is not one
anybody will play. The theoretical maximum under the 75-move and fivefold
automatic rules is roughly 17,700 plies, so this cap is not "every legal game" —
that was considered and rejected because it puts the bound near 28 MB.

`walk`'s unbounded `ply += 1` becomes unreachable, closing the u16 overflow with
no separate mechanism.

## 3. DrawClaim

```rust
DrawClaim {
    ply: u16,
    at: RecordId,
}
```

FIDE makes threefold (9.2) and fifty-move (9.3) a *claim*, not an automatic
result. `Status.repetitions` and `Status.halfmove_clock` already expose the
grounds; this adds the record type that lets a player act on them.

**Anchored to the head**, exactly like `DrawOffer`: a claim counts only while
`at` is still the head of the chain. This inherits invariant 9's no-race
property — the claimant is by definition the player to move, so they are the only
party who can advance the head, and the opponent cannot void the claim. It also
blocks the withheld-claim form of the non-monotonicity problem: a claim published
long after the fact does nothing.

**No `kind` field.** `walk` already returns `repetitions` and `pos.halfmoves()`
for the head position. Projection checks whether either ground actually holds
(≥3 repetitions, or ≥100 halfmoves) and reports which one fired via
`Reason::ThreefoldClaim` or `Reason::FiftyMoveClaim`. A claim with no valid
ground is ignored at projection, never fatal — invariant 1 applies to claims
exactly as it applies to illegal moves.

**Precedence** (invariant 7, amended):

```
forfeit > resignation > board result > draw claim > draw agreement
```

The board result must outrank the claim. The claimant is the player to move; if
that position is checkmate, the player to move is the one who has been mated.
Ranking the claim first would let a mated player draw their way out of a loss —
the same shape of bug the existing order already guards against by placing
resignation above the board.

## 4. Status.ignored

Currently `len - chain.len()`, which counts every resignation and draw record as
"ignored". Redefined as **the number of `Move` records whose id is not in the
chain** — illegal moves, wrong-parent moves, and records evicted from
consideration. That is the diagnostic a UI actually wants.

## 5. Wire format

Every change here rotates every record id and invalidates every signature.

| Rust | wire key | change |
|---|---|---|
| `Record.body` / `.signer` / `.sig` | `b` / `k` / `s` | unchanged |
| `Move.ply` / `.parent` / `.uci` | `p` / `t` / `u` | unchanged |
| `Resign` | `r` | unchanged |
| `DrawOffer.ply` / `.at` | `p` / `t` | **gains `p`** |
| `DrawAccept.ply` / `.offer` | `p` / `o` | **gains `p`** |
| `DrawClaim.ply` / `.at` | `c` → `p` / `t` | **new variant** |

### `ply` on a draw record is a grouping index only

Projection ignores it. Liveness of an offer, an acceptance or a claim is
determined entirely by `at` / `offer` against the current head, exactly as it is
today. Nothing checks that a draw record's `ply` corresponds to the head it
names, and nothing should: such a check would be a second source of truth for
liveness, and a wrong-but-honest `ply` would then silently void a legitimate
draw.

The field exists so eviction has an index to group on. A signer who puts
arbitrary plies on their offers merely spreads their own records across more of
their own groups — bounded by `MAX_PLY`, and costing them nothing but slots they
own.

### The delegate is unaffected

`GAME_RECORD_FORMAT` does **not** need a bump. `decide_sign` matches on the
`Body` variant rather than on a ply field, so draw bodies continue to fall
through to the unconditional-sign arm and adding `ply` to them does not engage
the one-signature-per-ply guard. `body_hash` is computed only over `Move`
bodies, whose encoding is unchanged, so `last_move_body_hash` stays stable
across this change and a legitimate retry still matches.

`decide_sign` does need a new match arm for `DrawClaim`, alongside `Resign`,
`DrawOffer` and `DrawAccept`: idempotent by record id, no guard required.

## 6. Testing

The law tests are the deliverable. New state-shape behaviour needs a law test,
not a happy-path test.

**Algebra (`common/tests/algebra.rs`):**
- top-K distributes over merge, under randomized partitions and delivery orders
- eviction is idempotent
- the bound holds: a signer spamming one group leaves the state at K for that
  group
- Property 1 holds post-eviction
- two peers converge after a bidirectional exchange when one holds a record the
  other evicts

**Adversarial (`common/tests/adversarial.rs`):**
- a spamming signer cannot evict the *opponent's* records
- an over-K group flooded with `DrawOffer` records does not evict `Move` records
  at the same ply (the `kind`-in-group-key property)
- a record with `ply > MAX_PLY` is refused by `verify`
- **the accepted weakening**: a cheater double-signs, buries both moves under
  lower-id illegal records, and the game stalls with no decision rather than
  forfeiting

**Projection (`common/tests/adversarial.rs`):**
- a valid threefold claim at the head draws the game
- a valid fifty-move claim at the head draws the game
- a claim with no valid ground is ignored, not fatal
- a stale claim (`at` is no longer the head) is ignored
- a claim at a checkmate position loses to the board result
- `Status.ignored` counts illegal moves but not resignations or draw records

**Contract (`contracts/adjourn-contract/tests/interface.rs`):**
- an over-K state round-trips through the real interface and comes back
  normalized

## 7. Documentation

`CLAUDE.md` changes required, beyond the wire table:

- Rewrite the eviction entry under known issues. The claim that "K=2 preserves
  the double-sign fraud proof" is false under this design.
- Amend invariant 7 with the claim tier and its rationale.
- Close the unbounded-growth and `u16` overflow entries.
- Close the `Status.ignored` entry.
- Amend the threefold/fifty-move entry: the claim record now exists; the open
  design question it names is resolved.
- Add a known-issues entry for the evadable forfeit.
- Update test counts.

## Out of scope

- The witness fraud-proof record (rejected above).
- `make_move` round-tripping through FEN, and returning `Chess` from `project`.
  Real, but an internal refactor that rotates no ids and can be done any time.
- The contract-key coupling between `adjourn-core` and the contract WASM.
- `watch`, and anything else in the CLI.
