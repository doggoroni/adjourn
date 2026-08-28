# Contract-key migration: carrying a game across a contract rebuild

**Status:** design approved 2026-08-28, not yet implemented.

**Goal.** Let a player move an in-progress game onto a rebuilt contract without
losing it, and make it impossible for the two players to end up silently on
different contract generations.

---

## The problem

The contract key is `ContractInstanceId::from_params_and_code(params, code)`.
Any change to the emitted WASM bytes — a rustc bump, a dependency patch, a
different `--remap-path-prefix` — moves the key for every game, even when
behaviour and the wire format are untouched.

Today that fails **closed and loudly**, in two places:

- `game_bind` (`client/src/session.rs:157`) refuses an offer whose named
  contract differs from the one this build derives.
- `bound_game` (`client/src/session.rs:232`) refuses any move-flow command when
  this build's derived id differs from the id the delegate recorded at bind
  time: *"Rebuild with the same adjourn-contract version used when this game
  was bound."*

Failing loudly is right and this design does not weaken it. But there is no way
forward: a correspondence game can last months, and the only current answer is
"keep the old binary until the game ends".

### Correction: what River actually does

`CLAUDE.md` said River derives its key as `blake3(code_hash ‖ owner_key)` "so
invite links survive upgrades". **That is wrong**, and this spec supersedes it.
River's own `common/src/migration.rs` states the opposite:

> The room contract key is `BLAKE3(room_contract.wasm, params)` with
> `params = { owner: VerifyingKey }`, so every room-contract WASM upgrade moves
> the key for every owner.

That is the standard Freenet derivation — the same one adjourn already uses.
Keys do not survive upgrades and no derivation makes them. River instead keeps
a registry of every previous WASM code hash
(`LEGACY_ROOM_CONTRACT_CODE_HASHES`, **31 generations** as of the vendored
checkout) and has clients probe old keys newest-to-oldest to recover dormant
rooms. Its migration module is feature-gated so it never compiles into the
contract WASM — the same byte-identity discipline adjourn applies to
package-scoped builds.

**adjourn needs no such registry.** River probes because its client knows only
the owner key. adjourn's delegate records each game's actual contract id, so
the previous address is known exactly, however many generations have passed.
Migration is a direct `GET(old) → PUT(new)`, not a search.

---

## Constraints

- **The wire format is frozen.** Records signed under an old build stay
  decodable and valid under a new one, because signatures bind `game_id`
  (derived from `GameParams`), never the code hash. A future wire-format change
  is out of scope and would need a different design.
- **Scope is migration from a live old contract.** If the old contract has gone
  cold and been evicted, report that and stop. Local state persistence is
  separate work and is deliberately not part of this.
- **`common/` must not change.** Any change there rotates the contract key,
  which is the problem being solved, not a step in solving it.

## Validated assumptions

Both confirmed against live `freenet 0.2.130` nodes on 2026-08-28, using two
builds of identical source distinguished only by `--remap-path-prefix`
(canonical `875ac4d2…`, 267,003 bytes; variant `3e55ca2e…`, 267,227 bytes):

- **A non-empty PUT to a contract id the network has never seen is accepted and
  round-trips.** The 15-ply cross-host game (16 records, 2,465 bytes, including
  a `Resign`) was PUT under the variant id and read back projecting identically
  at ply 15.
- **A rebuilt contract accepts records signed under the old build**, as the
  `game_id` argument predicts.

The first two probe runs timed out and were **not** evidence against this: an
empty-PUT control failed identically, which localised the cause to the gateway
host having gone offline rather than to the payload. Recorded because the
control is the only reason a false negative was not reported.

---

## Design

### Trigger

Explicit, never automatic. `bound_game`'s existing mismatch error gains a
pointer to `adjourn game migrate --label <label>`; the UI surfaces the same as
a button on the affected game. Automatic migration is rejected: it makes a
structural change to a game silently, and sometimes the right answer is to go
back to the old build instead.

### Flow

1. `GET(old_id)` — from the delegate's record. No registry, no search.
2. `PUT(new_container, old_state)` — new container built from the current WASM
   and the unchanged `GameParams`.
3. `Rebind` on the delegate — update the recorded contract id, store the
   previous one.
4. Watch **both** ids for the remainder of the game.

Both players derive the same new id independently from identical params, so
migration needs no coordination on the address.

### Delegate change

A new `Rebind` request carrying `{ label, contract: [u8; 32] }`. It updates
`GameRecord.contract` and sets a new field
`GameRecord.previous: Option<[u8; 32]>` — an id, matching how `origin` stores a
derived id rather than a richer type. It **preserves `last_signed_ply`,
`origin`, `params` and `game_id` unchanged**.

`GAME_RECORD_FORMAT` goes **2 → 3** for the added `previous` field. The reader
must migrate a format-2 record by treating `previous` as `None`. It must NOT
widen the format check.

This bump is load-bearing, not ceremony. `CLAUDE.md`'s "Persisted-record
versioning" section describes the exact failure a careless version causes: a
`#[serde(default)]` field lets serde decode an old record with
`last_signed_ply` silently reset to 0, disarming the double-sign guard on a
live game. `Rebind` is the change most likely to introduce it.

`Rebind` is **trust-the-client** by construction: the delegate cannot verify a
supplied id derives from the stored params, because it cannot hash WASM. This
is acceptable — the id check is a build-mismatch guard, not a security
boundary, and `Rebind` touches neither the signing key nor the ply counter. It
must be stated in the code, not left implicit.

### Where the code lives

- `common/src/delegate_policy.rs` — a pure `decide_rebind`, alongside the
  existing `decide_bind`/`decide_sign`. This is where it is host-testable on
  every platform; the delegate crate cannot be host-compiled on Windows.
- `delegates/adjourn-delegate/src/lib.rs` — a `handle_rebind` dispatch arm,
  generic over `SecretStore` like its siblings.
- `client/src/session.rs` — `pub async fn migrate_label`, following the shape
  of the other flows.
- `cli/src/main.rs` — `adjourn game migrate --label <label>`, parse-dispatch-
  render only.
- `ui/src/conn.rs` + `ui/src/views/game.rs` — a `Cmd::Migrate` and a button on
  the affected game.

**The delegate WASM changes, so the delegate key rotates.** That is survivable
by design and does not need solving here: `RegisterDelegate` carries a
`predecessors` list and the node copies LOCAL-scope secrets forward into the new
generation's namespace, which is the whole reason `GameRecord` carries a format
field. The contract WASM is untouched, so contract ids are unaffected by this
change itself.

### Skew detection

While `previous` is `Some`, the watch subscribes to both ids.

The signal is a **set difference**, not the presence of opponent records: the
old contract legitimately holds the whole pre-migration history. Records
present on the old id, absent from the new id, and signed by the opponent mean
they moved after the migration and have not come across. The message is
specific — *"your opponent is still on the previous contract version"* — rather
than a generic stall.

The rule is stateless: no stored migration ply that can drift, and it works
symmetrically to show the opponent has migrated.

`previous` is never cleared. Clearing it would need a second delegate mutation
to save one subscription on one bounded game, and `watch` already short-circuits
on a decided game.

### Failure handling

| Failure | Behaviour |
|---|---|
| Old contract evicted (`GET` → `None`) | Report plainly, do not `Rebind`. Not migratable. |
| `PUT` fails | No `Rebind`. Game unchanged, still on the old id. |
| `Rebind` fails after a successful `PUT` | New id holds state, delegate still points old. Re-running migrate completes it. |
| Already migrated | Derived id equals recorded id — no-op that says so. |

Migration is idempotent throughout: re-PUTting merges by union, and `Rebind` to
an already-current id is a no-op.

---

## Testing

Against `FakeNode`, two different WASM byte strings yield two ids, which is all
the migration path needs.

- Migration moves a game end to end: old state appears under the new id, the
  delegate's record updates, and the game remains playable.
- `Rebind` **preserves `last_signed_ply`** — its own named test.
- A format-2 record decodes with `previous = None`, and must **not** decode
  with `last_signed_ply` defaulted. Mirrors the existing
  `a_record_from_another_format_cannot_be_signed_against` and
  `the_format_check_precedes_every_other_check`.
- Skew: two fakes, one migrates, the opponent moves on the old id, the detector
  fires.
- Idempotency: migrating twice is a no-op the second time.

**Every one of these must be mutation-tested** — break the behaviour, confirm
the test fails, restore, `cargo clean -p <crate>`. This branch has already
produced two tests that passed without the fix they were named for, and
"preserves `last_signed_ply`" is exactly the shape of assertion that passes
vacuously.

---

## Out of scope

- Local state persistence and the abandonment case.
- Wire-format changes, which would require transforming records rather than
  moving them.
- Any change to `common/`.
- Automatic migration.

## Known risk

A migrated player whose opponent never upgrades has a game that cannot
progress. Detection makes that visible and specific, but the resolution is out
of band: the opponent upgrades, or the migrator returns to the old build. This
design does not attempt to arbitrate it, and should not — the players are the
only ones who can decide.
