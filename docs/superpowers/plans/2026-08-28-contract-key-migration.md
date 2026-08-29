# Contract-Key Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a player move an in-progress game onto a rebuilt contract without losing it, and make a half-migrated game impossible to miss.

**Architecture:** The delegate already records each game's contract id, so migration is a direct `GET(old) → PUT(new) → Rebind`, with no legacy-hash registry and no backward search. A new `previous` field on `GameRecord` (format 2 → 3, migrated not refused) lets the watch subscribe to both ids and report a skew as a set difference.

**Tech Stack:** Rust 1.97.1 (pinned), `adjourn-core` (no Freenet deps), `freenet-stdlib`, `ciborium` CBOR, `ed25519-dalek`, `tokio` (CLI), Dioxus 0.7 (UI).

**Spec:** `docs/superpowers/specs/2026-08-28-contract-key-migration-design.md`

## Global Constraints

> **SUPERSEDED, read this first.** The constraint below asserts the contract
> hash must remain `875ac4d2…`. That turned out to be false: adding ANY new
> `Request` variant rotates it, and this branch accepted the rotation
> deliberately. The current key is
> `15beda67aa32da2e3274d57ab190114ccf3b73785be980776333d6822691e506`. See
> `CLAUDE.md`, "Reproducible builds", for the measurement and the reasoning.
> What still holds is determinism: two builds of one source must agree.


- **Do not change the contract's reachable graph.** The contract imports only `adjourn_core::state::{Delta, Summary}` and `adjourn_core::{GameParams, GameState}`. Changes confined to `delegate_policy.rs` / `delegate_api.rs` are stripped and leave the contract byte-identical (verified: `875ac4d2619179339c7bd853d00154fc06f29844c793c2626e27bcbef1c69c2c`, 267,003 bytes). **Anything touching `GameParams`, `GameState`, `Record`, `Delta` or `Summary` rotates the contract key and is forbidden here.**
- **Verify the contract hash is unchanged** at the end of any task that edits `common/`: run `./scripts/build-contract.sh` and confirm the sha256 above.
- **Never run `cargo build --release` on the contract or delegate** — use `scripts/build-contract.sh` / `scripts/build-delegate.sh`.
- **Format 2 records must be MIGRATED, not refused.** A naive bump rejects every already-bound game.
- **`last_signed_ply` must survive every path.** A `#[serde(default)]` that lets it decode as 0 disarms the double-sign guard on a live game.
- **Windows cannot link `cargo test`, and cannot host-compile the delegate crate.** Use `wsl.exe -d Ubuntu -- bash -lc 'cd /mnt/c/Users/Tony/dev/freenet-chess && ...'`. Verify the delegate with `--target wasm32-unknown-unknown`.
- **Gates:** `cargo test --workspace --locked` (baseline **168**), `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo clippy -p adjourn-ui --target wasm32-unknown-unknown --locked -- -D warnings`, and both `adjourn-ui` build directions.
- **Mutation-test every new test**: break the behaviour, confirm the test fails, restore, then `cargo clean -p <crate>` before re-running. Stale artifacts have produced false results in this repo.
- **No `rand`/`getrandom` in `ui/` or the contract/delegate graphs.**

---

## File Structure

| File | Responsibility |
|---|---|
| `common/src/delegate_policy.rs` | `GameRecord.previous`, `GAME_RECORD_FORMAT = 3`, `migrate_record`, `decide_rebind` — all pure, host-testable on every platform |
| `common/src/delegate_api.rs` | `Request::Rebind`, `Refusal::NotBound`, `GameSummary.previous` |
| `common/tests/delegate_policy.rs` | format migration and rebind decision tests |
| `delegates/adjourn-delegate/src/secrets.rs` | `load_game` applies `migrate_record` — the single migration point |
| `delegates/adjourn-delegate/src/lib.rs` | `handle_rebind` dispatch arm |
| `delegates/adjourn-delegate/tests/adapter.rs` | rebind through the real dispatch path |
| `client/src/session.rs` | `migrate_label`, `opponent_moved_on_previous`, watch both ids |
| `client/tests/migrate.rs` | migration and skew against `FakeNode` |
| `cli/src/main.rs` | `adjourn game migrate --label <label>` |
| `ui/src/conn.rs`, `ui/src/views/game.rs` | `Cmd::Migrate` and a button |

---

## Task 1: `GameRecord.previous`, format 3, and migration on read

**Files:**
- Modify: `common/src/delegate_policy.rs:93` (`GAME_RECORD_FORMAT`), `:97-133` (`GameRecord`)
- Modify: `delegates/adjourn-delegate/src/secrets.rs:120` (`load_game`)
- Test: `common/tests/delegate_policy.rs`

**Interfaces:**
- Produces: `GameRecord.previous: Option<[u8; 32]>`; `GAME_RECORD_FORMAT: u8 = 3`; `pub fn migrate_record(rec: GameRecord) -> Option<GameRecord>`

- [ ] **Step 1: Write the failing tests**

Append to `common/tests/delegate_policy.rs`:

```rust
/// A format-2 record must be UPGRADED, not rejected. Refusing it would break
/// every game already bound — the games this feature exists to save.
#[test]
fn a_format_2_record_migrates_to_the_current_format() {
    let mut old = sample_record();
    old.format = 2;
    old.previous = None;
    old.last_signed_ply = 7;

    let migrated = migrate_record(old.clone()).expect("format 2 must migrate");
    assert_eq!(migrated.format, GAME_RECORD_FORMAT);
    assert_eq!(migrated.previous, None, "a format-2 record has no previous id");
}

/// The whole point of the format field. If migration silently zeroed this,
/// the double-sign guard would be disarmed on a live game.
#[test]
fn migration_preserves_last_signed_ply_and_every_other_field() {
    let mut old = sample_record();
    old.format = 2;
    old.last_signed_ply = 9;
    old.last_move_body_hash = [7u8; 32];

    let migrated = migrate_record(old.clone()).expect("format 2 must migrate");
    assert_eq!(migrated.last_signed_ply, 9, "ply counter must survive migration");
    assert_eq!(migrated.last_move_body_hash, [7u8; 32]);
    assert_eq!(migrated.label, old.label);
    assert_eq!(migrated.params, old.params);
    assert_eq!(migrated.side, old.side);
    assert_eq!(migrated.origin, old.origin);
    assert_eq!(migrated.contract, old.contract);
    assert_eq!(migrated.entropy, old.entropy);
}

/// Migrate the shapes we know; refuse the rest. Never widen the check.
#[test]
fn an_unknown_format_does_not_migrate() {
    for bad in [0u8, 1, 4, 255] {
        let mut rec = sample_record();
        rec.format = bad;
        assert!(
            migrate_record(rec).is_none(),
            "format {bad} must not migrate — widening this check is how a \
             future layout gets silently misread"
        );
    }
}

#[test]
fn a_current_format_record_passes_through_unchanged() {
    let rec = sample_record();
    assert_eq!(rec.format, GAME_RECORD_FORMAT);
    assert_eq!(migrate_record(rec.clone()), Some(rec));
}
```

Add this helper near the top of the same file if one does not already exist (check first — reuse the existing fixture builder rather than adding a second):

```rust
fn sample_record() -> GameRecord {
    GameRecord {
        format: GAME_RECORD_FORMAT,
        label: "alice".into(),
        params: sample_params(),
        side: Side::White,
        origin: None,
        contract: [3u8; 32],
        previous: None,
        entropy: EntropyQuality::HostBacked,
        last_signed_ply: 0,
        last_move_body_hash: [0u8; 32],
    }
}
```

- [ ] **Step 2: Run to verify failure**

```bash
wsl.exe -d Ubuntu -- bash -lc 'cd /mnt/c/Users/Tony/dev/freenet-chess && cargo test -p adjourn-core --test delegate_policy --locked 2>&1 | tail -20'
```

Expected: FAIL — `cannot find function migrate_record` and `no field previous`.

- [ ] **Step 3: Add the field and bump the format**

In `common/src/delegate_policy.rs`, change line 93 to `pub const GAME_RECORD_FORMAT: u8 = 3;` and add to `GameRecord`, immediately after `contract`:

```rust
    /// Contract instance id this game was bound to BEFORE a migration, or
    /// `None` if it has never been migrated.
    ///
    /// Kept so a client can keep watching the old address after moving a game
    /// to a rebuilt contract: if the opponent has not migrated, their moves
    /// keep landing there, and a game that silently stops advancing is the
    /// failure mode this project treats as the worst one.
    ///
    /// `#[serde(default)]` is safe HERE and nowhere near `last_signed_ply`:
    /// defaulting an id to `None` loses no safety property, while defaulting a
    /// ply counter to 0 disarms the double-sign guard.
    #[serde(with = "serde_bytes", default)]
    pub previous: Option<[u8; 32]>,
```

- [ ] **Step 4: Add `migrate_record`**

Add below the `impl GameRecord` block:

```rust
/// Bring a decoded record up to [`GAME_RECORD_FORMAT`], or refuse it.
///
/// The delegate's secret store is forward-carried across generations
/// (`RegisterDelegate` copies LOCAL secrets into the new namespace), so a
/// newer delegate WILL read records an older one wrote. Refusing an old shape
/// outright would strand every game already bound; silently accepting one
/// would risk reading a field that meant something else. So: migrate the
/// shapes we know, refuse everything else.
pub fn migrate_record(rec: GameRecord) -> Option<GameRecord> {
    match rec.format {
        GAME_RECORD_FORMAT => Some(rec),
        // v2 -> v3 added `previous`. Every other field carries over
        // unchanged — in particular `last_signed_ply`, which must never be
        // reset by a migration.
        2 => Some(GameRecord {
            format: GAME_RECORD_FORMAT,
            previous: None,
            ..rec
        }),
        _ => None,
    }
}
```

- [ ] **Step 5: Apply it at the single read point**

In `delegates/adjourn-delegate/src/secrets.rs`, change `load_game` so every reader sees a current-format record:

```rust
pub fn load_game<S: SecretStore>(store: &S, game_id: &GameId) -> Option<GameRecord> {
    let raw = store.get(&game_secret(game_id))?;
    let rec: GameRecord = ciborium::from_reader(raw.as_slice()).ok()?;
    // ONE migration point, so no caller can forget. The per-decision format
    // checks stay as defence in depth.
    migrate_record(rec)
}
```

Adjust to match the function's existing body — keep whatever decode and error handling is already there, and only add the `migrate_record` call on the way out. Import it from `adjourn_core::delegate_policy`.

- [ ] **Step 6: Fix every `GameRecord` construction site**

```bash
wsl.exe -d Ubuntu -- bash -lc 'cd /mnt/c/Users/Tony/dev/freenet-chess && cargo build --workspace --locked 2>&1 | grep -E "^error" -A 6 | head -40'
```

Add `previous: None` to each reported literal. In `decide_bind`'s `BindDecision::Bind { record: GameRecord { .. } }`, a freshly bound game has never migrated, so `previous: None` is correct there.

- [ ] **Step 7: Run tests**

```bash
wsl.exe -d Ubuntu -- bash -lc 'cd /mnt/c/Users/Tony/dev/freenet-chess && export CI=1 && cargo test --workspace --locked 2>&1 | grep -oE "ok\. [0-9]+ passed" | grep -oE "[0-9]+" | paste -sd+ | bc'
```

Expected: **172** (168 + 4 new).

- [ ] **Step 8: Mutation-test the ply-preservation assertion**

Temporarily change the `2 =>` arm to `last_signed_ply: 0,`. Re-run: `migration_preserves_last_signed_ply_and_every_other_field` MUST fail. Restore, then:

```bash
wsl.exe -d Ubuntu -- bash -lc 'cd /mnt/c/Users/Tony/dev/freenet-chess && cargo clean -p adjourn-core && export CI=1 && cargo test -p adjourn-core --test delegate_policy --locked 2>&1 | tail -3'
```

Record in your report that you did this and what you saw.

- [ ] **Step 9: Confirm the contract key did not move**

```bash
wsl.exe -d Ubuntu -- bash -lc 'cd /mnt/c/Users/Tony/dev/freenet-chess && ./scripts/build-contract.sh 2>&1 | grep -E "sha256|size"'
```

Expected exactly: `875ac4d2619179339c7bd853d00154fc06f29844c793c2626e27bcbef1c69c2c`, 267,003 bytes. **If this differs, stop and report it** — it means the change reached the contract's graph.

- [ ] **Step 10: Commit**

```bash
git add -A && git commit -m "feat(delegate): GameRecord.previous, format 3, migrated not refused"
```

---

## Task 2: `Request::Rebind` and `decide_rebind`

**Files:**
- Modify: `common/src/delegate_api.rs:40-69` (`Request`), `:126-148` (`Refusal`), `:100-120` (`GameSummary`)
- Modify: `common/src/delegate_policy.rs`
- Test: `common/tests/delegate_policy.rs`

**Interfaces:**
- Consumes: `GameRecord.previous`, `migrate_record` (Task 1)
- Produces: `Request::Rebind { label: String, contract: [u8; 32] }`; `Refusal::NotBound`; `pub enum RebindDecision { Rebind { record: GameRecord }, Refuse(Refusal) }`; `pub fn decide_rebind(existing: Option<&GameRecord>, label: &str, contract: [u8; 32], origin: Option<[u8; 32]>) -> RebindDecision`; `GameSummary.previous: Option<[u8; 32]>`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn rebind_updates_the_contract_and_records_the_previous_one() {
    let mut rec = sample_record();
    rec.contract = [1u8; 32];
    rec.last_signed_ply = 5;

    match decide_rebind(Some(&rec), "alice", [2u8; 32], None) {
        RebindDecision::Rebind { record } => {
            assert_eq!(record.contract, [2u8; 32]);
            assert_eq!(record.previous, Some([1u8; 32]));
            assert_eq!(record.last_signed_ply, 5, "the ply counter must survive a rebind");
            assert_eq!(record.params, rec.params);
            assert_eq!(record.side, rec.side);
            assert_eq!(record.origin, rec.origin);
        }
        other => panic!("expected Rebind, got {other:?}"),
    }
}

/// Idempotent: rebinding to the id already recorded changes nothing, and in
/// particular must NOT set `previous` to the current id and start watching
/// an address that is the same as the live one.
#[test]
fn rebinding_to_the_same_contract_is_a_no_op() {
    let mut rec = sample_record();
    rec.contract = [1u8; 32];
    rec.previous = None;

    match decide_rebind(Some(&rec), "alice", [1u8; 32], None) {
        RebindDecision::Rebind { record } => {
            assert_eq!(record.contract, [1u8; 32]);
            assert_eq!(record.previous, None, "a no-op rebind must not invent a previous id");
        }
        other => panic!("expected a no-op Rebind, got {other:?}"),
    }
}

#[test]
fn rebind_refuses_a_label_with_no_bound_game() {
    assert!(matches!(
        decide_rebind(None, "alice", [2u8; 32], None),
        RebindDecision::Refuse(Refusal::NotBound)
    ));
}

/// Same origin rule as every other call: a web-app game keeps full protection,
/// and a `None` game refuses any caller that presents an origin.
#[test]
fn rebind_refuses_a_different_origin() {
    let mut rec = sample_record();
    rec.origin = Some([9u8; 32]);
    assert!(matches!(
        decide_rebind(Some(&rec), "alice", [2u8; 32], Some([8u8; 32])),
        RebindDecision::Refuse(Refusal::WrongOrigin)
    ));
}

/// A record whose layout is not ours cannot be trusted field by field.
#[test]
fn rebind_refuses_an_unmigratable_format() {
    let mut rec = sample_record();
    rec.format = 200;
    assert!(matches!(
        decide_rebind(Some(&rec), "alice", [2u8; 32], None),
        RebindDecision::Refuse(Refusal::WrongFormat { .. })
    ));
}
```

Use whatever `Refusal::WrongFormat` variant already exists for the format case — check `decide_bind`'s format branch at `common/src/delegate_policy.rs:172` and reuse its exact variant and fields rather than inventing one.

- [ ] **Step 2: Run to verify failure**

```bash
wsl.exe -d Ubuntu -- bash -lc 'cd /mnt/c/Users/Tony/dev/freenet-chess && cargo test -p adjourn-core --test delegate_policy --locked 2>&1 | tail -15'
```

Expected: FAIL — `cannot find function decide_rebind`.

- [ ] **Step 3: Add the API types**

In `common/src/delegate_api.rs`, add to `Request` after `BindGame`:

```rust
    /// Point an already-bound game at a new contract instance, after the
    /// contract WASM was rebuilt and its key moved.
    ///
    /// The delegate CANNOT verify that `contract` really derives from the
    /// stored params — that is `hash(code, params)` and the delegate has no
    /// contract code and no way to hash it. This is deliberately
    /// trust-the-client: the id check is a build-mismatch guard, not a
    /// security boundary, and rebinding touches neither the signing key nor
    /// the ply counter. The properties that matter — one signature per
    /// `(game, ply)`, and the key never leaving the delegate — are unaffected.
    Rebind {
        label: String,
        #[serde(with = "serde_bytes")]
        contract: [u8; 32],
    },
```

Add to `Refusal`:

```rust
    /// The label exists but has no game bound to it, so there is nothing to
    /// point at a new contract.
    NotBound,
```

Add to `GameSummary`, after `entropy`:

```rust
    /// The contract this game was bound to before a migration, if any. A
    /// client watches it alongside the current one so an opponent still on the
    /// old generation is visible rather than looking like a stalled game.
    #[serde(with = "serde_bytes")]
    pub previous: Option<[u8; 32]>,
```

- [ ] **Step 4: Add the decision function**

In `common/src/delegate_policy.rs`:

```rust
#[derive(Debug, PartialEq, Eq)]
pub enum RebindDecision {
    Rebind { record: GameRecord },
    Refuse(Refusal),
}

/// Point `label`'s bound game at a new contract instance.
///
/// Everything that carries a security property is preserved verbatim:
/// `last_signed_ply` and `last_move_body_hash` (the double-sign guard),
/// `params` and `side` (who may sign what), and `origin` (who may call at
/// all). Only the address changes.
pub fn decide_rebind(
    existing: Option<&GameRecord>,
    label: &str,
    contract: [u8; 32],
    origin: Option<[u8; 32]>,
) -> RebindDecision {
    let Some(existing) = existing else {
        return RebindDecision::Refuse(Refusal::NotBound);
    };
    // Before anything else: if the layout is not ours, no field inside it can
    // be trusted — including the origin we are about to check.
    if existing.format != GAME_RECORD_FORMAT {
        return RebindDecision::Refuse(Refusal::WrongFormat {
            found: existing.format,
            expected: GAME_RECORD_FORMAT,
        });
    }
    if existing.origin != origin {
        return RebindDecision::Refuse(Refusal::WrongOrigin);
    }
    if existing.label != label {
        return RebindDecision::Refuse(Refusal::UnknownLabel);
    }
    // A no-op rebind must not record the current id as "previous" — that would
    // start a watch on an address identical to the live one.
    let previous = if existing.contract == contract {
        existing.previous
    } else {
        Some(existing.contract)
    };
    RebindDecision::Rebind {
        record: GameRecord {
            contract,
            previous,
            ..existing.clone()
        },
    }
}
```

Match `Refusal::WrongFormat`'s real field names to whatever `decide_bind` already uses.

- [ ] **Step 5: Run tests**

```bash
wsl.exe -d Ubuntu -- bash -lc 'cd /mnt/c/Users/Tony/dev/freenet-chess && export CI=1 && cargo test -p adjourn-core --test delegate_policy --locked 2>&1 | tail -4'
```

Expected: all pass. Workspace total **177** (172 + 5).

- [ ] **Step 6: Mutation-test**

Change `..existing.clone()` to construct with `last_signed_ply: 0`. `rebind_updates_the_contract_and_records_the_previous_one` MUST fail. Restore and `cargo clean -p adjourn-core`.

- [ ] **Step 7: Confirm the contract hash is unchanged, then commit**

```bash
wsl.exe -d Ubuntu -- bash -lc 'cd /mnt/c/Users/Tony/dev/freenet-chess && ./scripts/build-contract.sh 2>&1 | grep sha256'
git add -A && git commit -m "feat(delegate): decide_rebind and the Rebind request"
```

---

## Task 3: `handle_rebind` in the delegate

**Files:**
- Modify: `delegates/adjourn-delegate/src/lib.rs`
- Test: `delegates/adjourn-delegate/tests/adapter.rs`

**Interfaces:**
- Consumes: `decide_rebind`, `RebindDecision`, `Request::Rebind`, `Refusal::NotBound` (Task 2)
- Produces: `handle_rebind<S: SecretStore>(store: &mut S, label: &str, contract: [u8; 32], origin: Option<[u8; 32]>) -> Response`

- [ ] **Step 1: Write the failing test**

Append to `delegates/adjourn-delegate/tests/adapter.rs`:

```rust
/// Drive the REAL dispatch path, not just the policy beneath it: a bound game
/// is repointed, the previous id is kept, and the ply counter survives.
#[test]
fn rebind_repoints_a_bound_game_through_the_real_dispatch() {
    let mut store = MemoryStore::default();
    // Reuse this file's existing helper for creating + binding a game.
    let (label, game_id) = bind_a_game(&mut store, "alice", [1u8; 32]);

    let resp = adjourn_delegate::handle(
        &mut store,
        Request::Rebind { label: label.clone(), contract: [2u8; 32] },
        None,
    );
    assert!(matches!(resp, Response::Bound { .. }), "got {resp:?}");

    let rec = secrets::load_game(&store, &game_id).expect("record still there");
    assert_eq!(rec.contract, [2u8; 32]);
    assert_eq!(rec.previous, Some([1u8; 32]));
}

#[test]
fn rebind_refuses_a_label_that_was_never_bound() {
    let mut store = MemoryStore::default();
    let resp = adjourn_delegate::handle(
        &mut store,
        Request::Rebind { label: "nobody".into(), contract: [2u8; 32] },
        None,
    );
    assert!(matches!(resp, Response::Refused(Refusal::NotBound)), "got {resp:?}");
}
```

Match this file's existing helpers and `handle` signature exactly — read the neighbouring tests first and follow their shape rather than the sketch above.

- [ ] **Step 2: Run to verify failure**

```bash
wsl.exe -d Ubuntu -- bash -lc 'cd /mnt/c/Users/Tony/dev/freenet-chess && CI=1 cargo test -p adjourn-delegate --test adapter --locked 2>&1 | tail -12'
```

Expected: FAIL — no `Request::Rebind` arm in `handle`.

- [ ] **Step 3: Implement the handler**

In `delegates/adjourn-delegate/src/lib.rs`, following `handle_bind_game`'s shape:

```rust
pub fn handle_rebind<S: SecretStore>(
    store: &mut S,
    label: &str,
    contract: [u8; 32],
    origin: Option<[u8; 32]>,
) -> Response {
    // Ownership first, exactly as bind does: a label belongs to whoever
    // created it, and a caller that cannot prove that gets nothing.
    if secrets::load_owner(store, label) != origin {
        return Response::Refused(Refusal::WrongOrigin);
    }
    let Some(game_id) = secrets::load_bound_game_id(store, label) else {
        return Response::Refused(Refusal::NotBound);
    };
    let existing = secrets::load_game(store, &game_id);
    match decide_rebind(existing.as_ref(), label, contract, origin) {
        RebindDecision::Refuse(r) => Response::Refused(r),
        RebindDecision::Rebind { record } => {
            if !secrets::store_game(store, &record) {
                return Response::Refused(Refusal::NotBound);
            }
            Response::Bound { game_id }
        }
    }
}
```

Add the dispatch arm alongside the others:

```rust
        Request::Rebind { label, contract } => handle_rebind(store, &label, contract, origin_id),
```

Use whatever the existing arms call the origin parameter.

- [ ] **Step 4: Run tests**

```bash
wsl.exe -d Ubuntu -- bash -lc 'cd /mnt/c/Users/Tony/dev/freenet-chess && export CI=1 && cargo test --workspace --locked 2>&1 | grep -oE "ok\. [0-9]+ passed" | grep -oE "[0-9]+" | paste -sd+ | bc'
```

Expected: **179**.

- [ ] **Step 5: Verify the delegate still builds for wasm**

```bash
wsl.exe -d Ubuntu -- bash -lc 'cd /mnt/c/Users/Tony/dev/freenet-chess && cargo check -p adjourn-delegate --target wasm32-unknown-unknown --locked 2>&1 | tail -3'
```

- [ ] **Step 6: Mutation-test**

Make `handle_rebind` skip the `load_owner` check. `rebind_refuses_a_label_that_was_never_bound` may still pass — the test that must fail is a wrong-origin one. If neither existing test catches it, **add** a wrong-origin dispatch test, then restore and `cargo clean -p adjourn-delegate`.

- [ ] **Step 7: Rebuild the delegate WASM and commit**

```bash
wsl.exe -d Ubuntu -- bash -lc 'cd /mnt/c/Users/Tony/dev/freenet-chess && ./scripts/build-delegate.sh 2>&1 | grep -E "sha256|size"'
git add -A && git commit -m "feat(delegate): handle_rebind dispatch arm"
```

Record the new delegate hash in your report — **it is expected to change**, and that is fine: `RegisterDelegate` carries `predecessors` and the node forward-carries LOCAL secrets. The contract hash must still be `875ac4d2…`.

---

## Task 4: `migrate_label` in the client

**Files:**
- Modify: `client/src/session.rs`
- Create: `client/tests/migrate.rs`

**Interfaces:**
- Consumes: `Request::Rebind` (Task 2)
- Produces: `pub async fn migrate_label<N: NodeClient>(node: &mut N, label: &str, contract_wasm: Vec<u8>) -> anyhow::Result<MigrateOutcome>`; `pub enum MigrateOutcome { AlreadyCurrent { contract: [u8; 32] }, Migrated { from: [u8; 32], to: [u8; 32], records: usize } }`

- [ ] **Step 1: Write the failing test**

Create `client/tests/migrate.rs`:

```rust
mod common;

use adjourn_client::session::{migrate_label, MigrateOutcome};
// plus the same imports the neighbouring client tests use

/// Two different WASM byte strings give two contract ids, which is all the
/// migration path needs to exercise. FakeNode runs the real contract code
/// regardless of the bytes, so this models the id change without needing a
/// second real build.
#[tokio::test]
async fn migrating_moves_a_game_to_the_new_contract_id() {
    let Some(wasm) = common::contract_wasm() else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let mut variant = wasm.clone();
    variant.extend_from_slice(b"\0\0variant");

    let (mut alice, mut bob) = common::two_players().await;
    common::play(&mut alice, "alice", "e2e4", wasm.clone()).await;

    let before = adjourn_client::session::open_game_view(&mut bob, "bob", wasm.clone())
        .await
        .unwrap();

    let outcome = migrate_label(&mut bob, "bob", variant.clone()).await.unwrap();
    let MigrateOutcome::Migrated { from, to, records } = outcome else {
        panic!("expected Migrated, got {outcome:?}");
    };
    assert_eq!(from, before.contract);
    assert_ne!(to, before.contract, "the id must actually move");
    assert_eq!(records, before.state.records.len(), "every record must come across");

    // The game is playable at the new address under the new build.
    let after = adjourn_client::session::open_game_view(&mut bob, "bob", variant)
        .await
        .unwrap();
    assert_eq!(after.contract, to);
    assert_eq!(after.status.ply, before.status.ply);
}

#[tokio::test]
async fn migrating_twice_is_a_no_op_the_second_time() {
    let Some(wasm) = common::contract_wasm() else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let mut variant = wasm.clone();
    variant.extend_from_slice(b"\0\0variant");

    let (mut alice, mut bob) = common::two_players().await;
    common::play(&mut alice, "alice", "e2e4", wasm.clone()).await;

    migrate_label(&mut bob, "bob", variant.clone()).await.unwrap();
    let second = migrate_label(&mut bob, "bob", variant).await.unwrap();
    assert!(
        matches!(second, MigrateOutcome::AlreadyCurrent { .. }),
        "a second migrate must be a no-op, got {second:?}"
    );
}
```

`common::two_players()` and `common::play()` are shorthand for whatever setup the existing `client/tests/` files use — **read `client/tests/moves.rs` and reuse its actual helpers**; do not add duplicates.

- [ ] **Step 2: Run to verify failure**

```bash
wsl.exe -d Ubuntu -- bash -lc 'cd /mnt/c/Users/Tony/dev/freenet-chess && CI=1 cargo test -p adjourn-client --test migrate --locked 2>&1 | tail -12'
```

Expected: FAIL — `migrate_label` not found.

- [ ] **Step 3: Implement**

In `client/src/session.rs`:

```rust
/// What a migration did, so a caller can report it precisely rather than
/// saying "ok".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrateOutcome {
    /// This build already derives the recorded id. Nothing to do.
    AlreadyCurrent { contract: [u8; 32] },
    Migrated { from: [u8; 32], to: [u8; 32], records: usize },
}

/// Move an in-progress game onto the contract id THIS build derives.
///
/// Ordered so a failure never leaves a worse state than it found: the PUT
/// happens before the delegate is told anything, so a failed PUT leaves the
/// game exactly where it was. If the PUT succeeds and the Rebind does not, the
/// new address holds the state and the delegate still points at the old one —
/// re-running this completes it, because a PUT of the same records merges by
/// union and a Rebind to the current id is a no-op.
pub async fn migrate_label<N: NodeClient>(
    node: &mut N,
    label: &str,
    contract_wasm: Vec<u8>,
) -> anyhow::Result<MigrateOutcome> {
    let summary = summary_for(node, label).await?;
    let params = summary
        .params
        .clone()
        .ok_or_else(|| anyhow!("{label} has a key but no bound game to migrate"))?;
    let old = summary
        .game_id
        .map(|_| summary.contract_of_record())
        .ok_or_else(|| anyhow!("{label} is not bound to a contract"))?;

    let (container, new_id) = contract_container(contract_wasm, &params)?;
    if *new_id == old {
        return Ok(MigrateOutcome::AlreadyCurrent { contract: old });
    }

    // Read the game from where it lives now. Scoped deliberately: if the old
    // contract has gone cold there is no local copy to fall back on, and
    // saying so is better than PUTting an empty state over the new address.
    let raw = node
        .get(ContractInstanceId::new(old), false)
        .await
        .context("GET the old contract")?
        .ok_or_else(|| {
            anyhow!(
                "the previous contract {} is no longer on the network, so this \
                 game cannot be migrated",
                ContractInstanceId::new(old).encode()
            )
        })?;
    let state = GameState::decode(&raw)
        .ok_or_else(|| anyhow!("the previous contract's state did not decode"))?;
    let records = state.records.len();

    node.put(container, state.encode())
        .await
        .context("PUT the game under the new contract id")?;

    match node
        .delegate(Request::Rebind { label: label.to_string(), contract: *new_id })
        .await
        .context("Rebind")?
    {
        Response::Bound { .. } => {}
        Response::Refused(r) => return Err(refused(r)),
        other => bail!("unexpected response to Rebind: {other:?}"),
    }
    Ok(MigrateOutcome::Migrated { from: old, to: *new_id, records })
}
```

`summary_for` / `contract_of_record` stand for however this module already turns a label into its `GameSummary` and contract id — **reuse the existing helper** (`bound_game` and its neighbours) rather than adding a parallel path.

- [ ] **Step 4: Point the mismatch error at the fix**

In `bound_game`'s build-mismatch `bail!` (`client/src/session.rs:234`), append to the message:

```
             version used when this game was bound, or run \
             `adjourn game migrate --label {label}` to move this game onto \
             the contract this build derives.
```

Thread `label` in if it is not already in scope.

- [ ] **Step 5: Run tests**

Expected workspace total: **181**.

- [ ] **Step 6: Mutation-test**

Delete the `if *new_id == old` early return. `migrating_twice_is_a_no_op_the_second_time` MUST fail. Restore and `cargo clean -p adjourn-client`.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(client): migrate_label moves a game to a rebuilt contract"
```

---

## Task 5: Skew detection

**Files:**
- Modify: `client/src/session.rs` (`watch_label`)
- Test: `client/tests/migrate.rs`

**Interfaces:**
- Consumes: `GameSummary.previous` (Task 2), `migrate_label` (Task 4)
- Produces: `pub fn opponent_moved_on_previous(current: &GameState, previous: &GameState, ours: &KeyBytes) -> bool`

- [ ] **Step 1: Write the failing test**

```rust
/// The old contract legitimately holds the whole pre-migration history, so
/// "the opponent has records there" is ALWAYS true and is not the signal. The
/// signal is a set difference: records on the old id that are absent from the
/// new one and were signed by the opponent.
#[test]
fn skew_is_a_set_difference_not_the_presence_of_opponent_records() {
    let (ours, theirs) = fixture_two_keys();
    let shared = state_with(&[(ours, 1), (theirs, 2)]);

    // Identical sets: migration complete, nothing to report.
    assert!(!opponent_moved_on_previous(&shared, &shared, &ours));

    // Opponent moved on the OLD contract after we migrated.
    let old_ahead = state_with(&[(ours, 1), (theirs, 2), (theirs, 4)]);
    assert!(opponent_moved_on_previous(&shared, &old_ahead, &ours));

    // WE are ahead on the old one. Not skew — it is our own record, and
    // reporting it would cry wolf on every migration.
    let ours_ahead = state_with(&[(ours, 1), (theirs, 2), (ours, 3)]);
    assert!(!opponent_moved_on_previous(&shared, &ours_ahead, &ours));
}
```

`fixture_two_keys()` and `state_with()` build signed records; follow the fixture style already used in `common/tests/adversarial.rs`.

- [ ] **Step 2: Run to verify failure**

Expected: FAIL — `opponent_moved_on_previous` not found.

- [ ] **Step 3: Implement the pure predicate**

In `client/src/session.rs`:

```rust
/// Has the opponent moved on the contract we migrated AWAY from?
///
/// Deliberately a set difference rather than "are there opponent records on
/// the old contract" — the old contract holds the entire pre-migration
/// history, all of it signed by both players, so that question is always yes.
/// What matters is a record present there and absent here, signed by them.
///
/// Stateless on purpose: no stored migration ply that can drift out of sync
/// with what actually got copied.
pub fn opponent_moved_on_previous(
    current: &GameState,
    previous: &GameState,
    ours: &KeyBytes,
) -> bool {
    previous
        .records
        .iter()
        .any(|(id, rec)| rec.signer != *ours && !current.records.contains_key(id))
}
```

- [ ] **Step 4: Watch both ids**

In `watch_label`, after the subscribing GET on the current contract, if the summary carries `previous: Some(old)`, also `node.get(old, true)`. On each wake, GET the old id, decode it, and if `opponent_moved_on_previous` is true, surface it once through the existing error/report channel with:

```
your opponent is still on the previous contract version — your moves are not
reaching them. Both players must run the same adjourn-contract build.
```

Do not tear the watch down: the game is still readable and the player may want to go back to the old build.

- [ ] **Step 5: Run tests**

Expected workspace total: **182**.

- [ ] **Step 6: Mutation-test**

Change the predicate to `rec.signer == *ours`. The "we are ahead" case MUST start failing. Restore and `cargo clean -p adjourn-client`.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(client): detect an opponent left on the previous contract"
```

---

## Task 6: CLI and UI surfaces

**Files:**
- Modify: `cli/src/main.rs`, `ui/src/conn.rs`, `ui/src/views/game.rs`, `ui/index.html`

**Interfaces:**
- Consumes: `migrate_label`, `MigrateOutcome` (Task 4)
- Produces: `adjourn game migrate --label <label>`; `Cmd::Migrate { label: String }`

- [ ] **Step 1: Add the CLI subcommand**

In `cli/src/main.rs`, alongside the other `game` subcommands — parse, dispatch, render only:

```rust
        ("game", "migrate") => {
            let label = require_label(&args)?;
            match session::migrate_label(&mut node, &label, contract_wasm).await? {
                MigrateOutcome::AlreadyCurrent { contract } => println!(
                    "{label} is already on contract {} — nothing to migrate",
                    ContractInstanceId::new(contract).encode()
                ),
                MigrateOutcome::Migrated { from, to, records } => {
                    println!(
                        "{label}: moved {records} record(s)\n  from {}\n  to   {}",
                        ContractInstanceId::new(from).encode(),
                        ContractInstanceId::new(to).encode()
                    );
                    println!(
                        "Your opponent must run the same build. Until they do, \
                         your moves will not reach them."
                    );
                }
            }
        }
```

Match the file's existing dispatch and argument-parsing style exactly.

- [ ] **Step 2: Add the UI command**

In `ui/src/conn.rs`, add `Migrate { label: String }` to `Cmd` and an arm in the main actor mirroring `Cmd::Bind`'s shape, refreshing `view` afterwards as its siblings do.

- [ ] **Step 3: Add the button**

In `ui/src/views/game.rs`, render a migrate button only when `wires.error` reports a build mismatch for this label. Add a `.migrate` rule to `ui/index.html` beside the existing `.error` styles.

- [ ] **Step 4: Gates**

```bash
wsl.exe -d Ubuntu -- bash -lc 'cd /mnt/c/Users/Tony/dev/freenet-chess && export CI=1 && cargo test --workspace --locked 2>&1 | grep -oE "ok\. [0-9]+ passed" | grep -oE "[0-9]+" | paste -sd+ | bc && cargo fmt --all -- --check && cargo clippy --workspace --all-targets --locked -- -D warnings 2>&1 | tail -1 && cargo clippy -p adjourn-ui --target wasm32-unknown-unknown --locked -- -D warnings 2>&1 | tail -1 && cargo check -p adjourn-ui --target wasm32-unknown-unknown --locked 2>&1 | tail -1 && cargo check -p adjourn-ui --all-targets --locked 2>&1 | tail -1'
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: adjourn game migrate, and a migrate button in the UI"
```

---

## Task 7: Documentation

**Files:**
- Modify: `CLAUDE.md`, `docs/runbook-two-nodes.md`

- [ ] **Step 1: Correct the River claim in `CLAUDE.md`**

Find the line stating River derives its key as `blake3(code_hash ‖ owner_key)` "so invite links survive upgrades" and replace it. River's `common/src/migration.rs` says every WASM upgrade moves the key for every owner; it keeps a registry of 31 legacy code hashes and probes them backwards. adjourn needs no registry because its delegate records each game's actual contract id.

- [ ] **Step 2: Document the feature**

Add to `CLAUDE.md`'s "Delegate" section: `GameRecord` is format 3; `previous` holds the pre-migration id; format 2 is migrated in `load_game` via `migrate_record`, never refused, because refusing would strand every already-bound game. Note that `Rebind` is trust-the-client and why that is acceptable.

Add to the "Known issues" section: a migrated player whose opponent never upgrades has a game that cannot progress; detection makes it visible, resolution is out of band.

- [ ] **Step 3: Record the empirical finding**

Add to "Reproducible builds": delegate-only changes to `common/` leave the contract byte-identical, because the contract imports only `state::{Delta, Summary}` and `{GameParams, GameState}`. Verified 2026-08-28 — adding a `pub fn` to `delegate_policy.rs` reproduced `875ac4d2…` / 267,003 bytes exactly. Anything touching `GameParams`, `GameState`, `Record`, `Delta` or `Summary` still rotates the key.

- [ ] **Step 4: Update the test count**

Replace the "161 tests" / "168 tests" figure with the number read off the final run, with the per-binary split.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "docs: contract-key migration, and correct the River claim"
```

---

## Self-Review

**Spec coverage.** Trigger (T4 step 4, T6). Flow GET/PUT/Rebind (T4). Delegate `Rebind` + preservation (T2, T3). Format 2→3 migrated not refused (T1). `previous` field (T1). Skew as set difference (T5). `previous` never cleared — no task clears it, by design. Failure table (T4 step 3). Testing incl. mutation (every task). Contract-key non-rotation (T1 step 9, T7 step 3). Out-of-scope items have no tasks, correctly.

**Placeholders.** None. Three places say "reuse the existing helper" rather than inventing a parallel one — deliberate, and each names the file to read.

**Type consistency.** `previous: Option<[u8; 32]>` throughout. `MigrateOutcome` fields match between T4's definition, its tests, and T6's render. `decide_rebind`'s signature matches its call in T3. `opponent_moved_on_previous` takes `(&GameState, &GameState, &KeyBytes)` in both definition and test.

**Test count arithmetic:** 168 → 172 (T1) → 177 (T2) → 179 (T3) → 181 (T4) → 182 (T5). Treat as expected values, not assertions; report what the run actually says.
