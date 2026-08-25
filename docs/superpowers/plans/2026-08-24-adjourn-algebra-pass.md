# Bounded-State Algebra Pass Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bound contract state with top-K eviction and a structural ply cap, and add the `DrawClaim` record type so FIDE's threefold and fifty-move claims are actionable.

**Architecture:** Merge becomes `A ⊕ B = topK(A ∪ B)`, keeping the K smallest record ids per `(signer, kind, ply)` group — top-K distributes over union, so the monoid laws survive. A structural `MAX_PLY` bounds the number of groups and therefore the state. `DrawClaim` is anchored to the head exactly like `DrawOffer`, inheriting its no-race property.

**Tech Stack:** Rust 1.97.1 (pinned), `shakmaty` 0.30.1, `ed25519-dalek` 2.2.0, `ciborium` (CBOR), `serde_bytes`.

**Spec:** `docs/superpowers/specs/2026-08-24-adjourn-algebra-pass-design.md`

## Global Constraints

- **`MAX_PLY: u16 = 4096`.** Enforced structurally in `Record::verify` — a record whose body carries a `ply` greater than this is not valid.
- **Eviction group key is `(signer, Kind, ply)`.** `Kind` is mandatory in the key: without it a player could flood `DrawOffer` records to evict their own `Move` records at the same ply, including both halves of a double-sign fraud proof.
- **K = 2 for `Move`, K = 1 for `DrawOffer` / `DrawAccept` / `DrawClaim`.** `Resign` is a unit variant — one possible body per signer, hence one possible id — and is never evicted.
- **`all_valid` (the contract's `validate_state`) must NOT reject an over-K state.** Rejecting is a content judgment of the kind invariant 1 forbids. Eviction happens at merge, which is every write path.
- **`BTreeMap`/`BTreeSet`, never `HashMap`/`HashSet`.** Canonical iteration order gives byte-identical serialization across peers.
- **Byte fields carry `#[serde(with = "serde_bytes")]`.** A `[u8; 32]` costs 34 bytes with it and ~55 without.
- **`GAME_RECORD_FORMAT` is NOT bumped.** `body_hash` is computed only over `Move` bodies, whose encoding is unchanged.
- **Every task must leave `cargo test --workspace --locked` green.** Run it before every commit.
- **Do not run `cargo build --release` on the contract or delegate.** Use `scripts/build-contract.sh` / `scripts/build-delegate.sh`; a bare release build embeds home-directory paths and rotates the key.

---

### Task 1: Record shape — ply on draw bodies, `DrawClaim`, and `MAX_PLY`

**Files:**
- Modify: `common/src/types.rs`
- Modify: `common/src/project.rs` (destructuring call sites only)
- Modify: `common/src/delegate_policy.rs:269` (add `DrawClaim` to the sign arm)
- Modify: `cli/src/session.rs:437,467,479`
- Test: `common/tests/delegate_policy.rs` (CBOR round-trips), `common/tests/adversarial.rs`

**Interfaces:**
- Produces: `adjourn_core::types::MAX_PLY: u16`; `adjourn_core::types::Kind` (`Move`/`Resign`/`DrawOffer`/`DrawAccept`/`DrawClaim`, deriving `Ord`) with `Kind::k(self) -> usize`; `Body::kind(&self) -> Kind`; `Body::ply(&self) -> Option<u16>`; `Body::DrawClaim { ply: u16, at: RecordId }`; `ply: u16` added to `Body::DrawOffer` and `Body::DrawAccept`.
- Consumes: nothing.

- [ ] **Step 1: Write the failing tests**

Add to `common/tests/adversarial.rs`:

```rust
#[test]
fn a_record_beyond_max_ply_is_not_valid() {
    let (w, _b, params) = keys();
    let ok = Record::sign(
        &w,
        &params,
        Body::Move { ply: adjourn_core::types::MAX_PLY, parent: params.genesis(), uci: "e2e4".into() },
    );
    assert!(ok.verify(&params), "a record at exactly MAX_PLY must stay valid");

    let too_far = Record::sign(
        &w,
        &params,
        Body::Move { ply: adjourn_core::types::MAX_PLY + 1, parent: params.genesis(), uci: "e2e4".into() },
    );
    assert!(!too_far.verify(&params), "ply > MAX_PLY must be structurally invalid");

    // The cap is structural, so it must also refuse a state built from such a
    // record -- not merely ignore it at projection.
    let mut state = GameState::empty();
    state.insert_verified(&too_far, &params);
    assert!(state.is_empty(), "an over-MAX_PLY record must never enter state");
}

#[test]
fn the_ply_cap_applies_to_draw_records_too() {
    let (w, _b, params) = keys();
    let rec = Record::sign(
        &w,
        &params,
        Body::DrawOffer { ply: adjourn_core::types::MAX_PLY + 1, at: params.genesis() },
    );
    assert!(!rec.verify(&params), "draw records carry a ply and are capped too");
}

#[test]
fn resign_has_no_ply_and_one_possible_id() {
    let (w, _b, params) = keys();
    let a = Record::sign(&w, &params, Body::Resign);
    let b = Record::sign(&w, &params, Body::Resign);
    assert_eq!(a.body.ply(), None, "Resign is a unit variant: no ply to group on");
    assert_eq!(a.id(), b.id(), "one signer has exactly one possible Resign id");
}
```

Add to `common/tests/delegate_policy.rs`:

```rust
#[test]
fn the_new_and_changed_bodies_round_trip_through_cbor() {
    let (w, _b, params) = keys();
    for body in [
        Body::DrawOffer { ply: 7, at: [3u8; 32] },
        Body::DrawAccept { ply: 8, offer: [4u8; 32] },
        Body::DrawClaim { ply: 9, at: [5u8; 32] },
    ] {
        let rec = Record::sign(&w, &params, body.clone());
        let mut buf = Vec::new();
        ciborium::into_writer(&rec, &mut buf).expect("encode");
        let back: Record = ciborium::from_reader(&buf[..]).expect("decode");
        assert_eq!(rec, back);
        assert_eq!(back.body, body);
        assert!(back.verify(&params), "round-tripped record must still verify");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p adjourn-core --locked`
Expected: compile failure — `MAX_PLY` not found, `Body::DrawClaim` not found, `Body::ply` not found, and `Body::DrawOffer` has no field `ply`.

- [ ] **Step 3: Add `MAX_PLY`, `Kind`, and the body changes**

In `common/src/types.rs`, after the `KeyBytes` type alias:

```rust
/// The largest ply any record may carry, checked structurally in
/// [`Record::verify`].
///
/// This is what bounds the NUMBER of eviction groups, and therefore the state:
/// 5 records per signer per ply (2 moves + 1 each of three draw kinds), 10 per
/// ply across both players, so ~41,000 records or ~6.4 MB worst case.
///
/// 4096 plies is 2048 full moves. The longest recorded competitive game is 269
/// moves, so this cannot bind on real play. It is deliberately NOT the
/// theoretical maximum (~17,700 plies under the 75-move and fivefold automatic
/// rules), which would put the bound near 28 MB.
///
/// It also closes `walk`'s unbounded `ply += 1`: no record beyond the cap can
/// exist, so no chain can reach it.
pub const MAX_PLY: u16 = 4096;

/// Which kind of statement a body is, used as part of the eviction group key.
///
/// Separating kinds is load-bearing, not tidiness. Were groups keyed on
/// `(signer, ply)` alone, a player could flood `DrawOffer` records at ply N to
/// evict their own `Move` records at ply N -- including both halves of a
/// double-sign fraud proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Move,
    Resign,
    DrawOffer,
    DrawAccept,
    DrawClaim,
}

impl Kind {
    /// How many records one signer may hold in one `(signer, kind, ply)` group.
    pub fn k(self) -> usize {
        match self {
            // Both halves of a double-sign, so the fraud proof survives an
            // honest merge. See the spec on why this is not proof against a
            // determined self-spammer.
            Kind::Move => 2,
            // At a given ply there is exactly one head, so exactly one
            // legitimate `at`. K=1 costs an honest player nothing.
            _ => 1,
        }
    }
}
```

Change the `Body` enum — `Move` and `Resign` are untouched:

```rust
    /// A draw offer anchored to a specific head, so it expires implicitly
    /// once the game moves on.
    ///
    /// `ply` is a grouping index for eviction ONLY. Projection ignores it and
    /// keys liveness off `at`; checking the two against each other would make
    /// a second source of truth for liveness, and a wrong-but-honest `ply`
    /// would then silently void a legitimate draw.
    #[serde(rename = "o")]
    DrawOffer {
        #[serde(rename = "p")]
        ply: u16,
        #[serde(rename = "t", with = "serde_bytes")]
        at: RecordId,
    },
    /// Accepts a specific offer by record id. `ply` is a grouping index only,
    /// as for `DrawOffer`.
    #[serde(rename = "a")]
    DrawAccept {
        #[serde(rename = "p")]
        ply: u16,
        #[serde(rename = "o", with = "serde_bytes")]
        offer: RecordId,
    },
    /// Claims a draw by threefold repetition (FIDE 9.2) or the fifty-move rule
    /// (9.3), anchored to the head like `DrawOffer`.
    ///
    /// Carries no claim kind: projection already knows the repetition count and
    /// halfmove clock at the head, so it checks whether EITHER ground holds and
    /// reports which one fired.
    #[serde(rename = "c")]
    DrawClaim {
        #[serde(rename = "p")]
        ply: u16,
        #[serde(rename = "t", with = "serde_bytes")]
        at: RecordId,
    },
```

Add the accessors after the `Body` enum:

```rust
impl Body {
    /// The ply this body is indexed at, for eviction grouping.
    ///
    /// `Resign` has none, and needs none: it is a unit variant, so one signer
    /// has exactly one possible `Resign` body and therefore one possible id.
    pub fn ply(&self) -> Option<u16> {
        match self {
            Body::Move { ply, .. }
            | Body::DrawOffer { ply, .. }
            | Body::DrawAccept { ply, .. }
            | Body::DrawClaim { ply, .. } => Some(*ply),
            Body::Resign => None,
        }
    }

    pub fn kind(&self) -> Kind {
        match self {
            Body::Move { .. } => Kind::Move,
            Body::Resign => Kind::Resign,
            Body::DrawOffer { .. } => Kind::DrawOffer,
            Body::DrawAccept { .. } => Kind::DrawAccept,
            Body::DrawClaim { .. } => Kind::DrawClaim,
        }
    }
}
```

- [ ] **Step 4: Enforce the cap in `Record::verify`**

In `common/src/types.rs`, inside `verify`, immediately after the `params.color_of` check and before the signature length check:

```rust
        // Structural, and deliberately before any signature work: a pure
        // per-record predicate, so it distributes over merge and cannot cause
        // the partial-state divergence a chain-length-dependent rule would.
        if self.body.ply().is_some_and(|p| p > MAX_PLY) {
            return false;
        }
```

- [ ] **Step 5: Export the new items**

In `common/src/lib.rs`, change the `types` re-export line to:

```rust
pub use types::{color_at_ply, Body, GameParams, KeyBytes, Kind, Record, RecordId, MAX_PLY};
```

- [ ] **Step 6: Fix the destructuring call sites**

In `common/src/project.rs`, `draw_agreed` destructures two draw bodies. Add `..` so the new field is ignored — projection must not read `ply`:

```rust
        let Body::DrawAccept { offer, .. } = &rec.body else {
            continue;
        };
```

```rust
        let Body::DrawOffer { at, .. } = &offer_rec.body else {
            continue;
        };
```

In `common/src/delegate_policy.rs:269`, add `DrawClaim` to the unconditional-sign arm:

```rust
        // Idempotent by record id, so no guard is needed.
        Body::Resign
        | Body::DrawOffer { .. }
        | Body::DrawAccept { .. }
        | Body::DrawClaim { .. } => SignDecision::Sign {
            updated: record.clone(),
        },
```

In `cli/src/session.rs`, three sites. At line 437:

```rust
        Body::DrawOffer { ply: g.status.ply, at },
```

At line 467:

```rust
            matches!(&rec.body, Body::DrawOffer { at, .. } if *at == head)
```

At line 479:

```rust
        Body::DrawAccept { ply: g.status.ply, offer },
```

- [ ] **Step 7: Fix the existing tests that construct draw bodies**

Six sites construct `DrawOffer`/`DrawAccept` and need a `ply`. The value is a grouping index only, so any ply works, but use the ply the record logically belongs to so the tests stay readable:

- `common/tests/adversarial.rs:160-161` — offer anchored at `chain[1]`, so `ply: 2`
- `common/tests/adversarial.rs:182-183` — offer at the head, so `ply: chain.len() as u16`
- `common/tests/adversarial.rs:213-214` — offer at the head, so `ply: chain.len() as u16`
- `common/tests/algebra.rs:356-357` — offer at the head, so `ply: status.ply`
- `common/tests/algebra.rs:371-372` — offer at the head, so `ply: status.ply`
- `common/tests/delegate_policy.rs:501-502` — arbitrary; use `ply: 1` and `ply: 2`

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --workspace --locked`
Expected: PASS, including the three new tests from Step 1.

- [ ] **Step 9: Commit**

```bash
git add common/src/types.rs common/src/lib.rs common/src/project.rs common/src/delegate_policy.rs cli/src/session.rs common/tests/
git commit -m "feat(core)!: ply on draw bodies, DrawClaim variant, structural MAX_PLY"
```

---

### Task 2: Top-K eviction

**Files:**
- Modify: `common/src/state.rs`
- Test: `common/tests/algebra.rs`, `common/tests/adversarial.rs`, `contracts/adjourn-contract/tests/interface.rs`

**Interfaces:**
- Consumes: `Body::kind() -> Kind`, `Body::ply() -> Option<u16>`, `Kind::k() -> usize` from Task 1.
- Produces: `GameState::evict(&mut self)` (public, so tests can build an un-evicted state and normalize it explicitly).

- [ ] **Step 1: Write the failing law tests**

Add this helper near the top of `common/tests/algebra.rs`, after `play`:

```rust
/// `n` structurally-valid records from one signer in one `(signer, kind, ply)`
/// group. Varying `parent` gives distinct bodies -- hence distinct ids -- while
/// `verify` stays true, since it does not check the parent link.
fn spam_moves(key: &SigningKey, params: &GameParams, ply: u16, n: usize) -> Vec<Record> {
    (0..n)
        .map(|i| {
            let mut parent = [0u8; 32];
            parent[..8].copy_from_slice(&(i as u64).to_le_bytes());
            Record::sign(key, params, Body::Move { ply, parent, uci: "e2e4".into() })
        })
        .collect()
}

/// A state holding exactly these records, with NO eviction applied.
fn raw(records: &[Record]) -> GameState {
    let mut s = GameState::empty();
    for r in records {
        s.absorb_for_test(r);
    }
    s
}
```

Then the laws:

```rust
#[test]
fn eviction_bounds_a_spammed_group() {
    let (w, _b, params) = keys();
    let spam = spam_moves(&w, &params, 1, 50);
    let mut state = GameState::empty();
    state.merge(&raw(&spam), &params);
    assert_eq!(state.len(), 2, "Move groups are capped at K=2");
}

#[test]
fn eviction_distributes_over_merge() {
    let (w, b, params) = keys();
    let mut spam = spam_moves(&w, &params, 1, 25);
    spam.extend(spam_moves(&b, &params, 2, 25));

    // The law only bites when peers hold different fragments, so partition
    // randomly and repeatedly rather than at one fixed point.
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let mut whole = raw(&spam);
    whole.evict();

    for _ in 0..64 {
        let mut left = Vec::new();
        let mut right = Vec::new();
        for rec in &spam {
            if rng.below(2) == 0 {
                left.push(rec.clone());
            } else {
                right.push(rec.clone());
            }
        }

        // topK(topK(A) ∪ topK(B)) must equal topK(A ∪ B).
        let mut split = GameState::empty();
        split.merge(&raw(&left), &params);
        split.merge(&raw(&right), &params);

        assert_eq!(
            split.records, whole.records,
            "top-K must distribute over union for every partition"
        );
    }
}

#[test]
fn eviction_is_idempotent() {
    let (w, _b, params) = keys();
    let mut state = GameState::empty();
    state.merge(&raw(&spam_moves(&w, &params, 1, 30)), &params);
    let once = state.clone();
    state.evict();
    assert_eq!(once.records, state.records);
}

#[test]
fn merge_with_eviction_stays_commutative_and_associative() {
    let (w, b, params) = keys();
    let a = spam_moves(&w, &params, 1, 9);
    let c = spam_moves(&b, &params, 2, 9);
    let d = spam_moves(&w, &params, 3, 9);

    // `merged` evicts, so these are already normalized -- no extra evict call.
    let abc = raw(&a).merged(&raw(&c), &params).merged(&raw(&d), &params);
    let cba = raw(&d).merged(&raw(&c), &params).merged(&raw(&a), &params);
    assert_eq!(abc.records, cba.records, "order of merges must not matter");
}

#[test]
fn a_spammer_cannot_evict_the_opponents_records() {
    let (state, params, w, _b) = play(&["e2e4", "e7e5"]);
    let black_move = state
        .records
        .iter()
        .find(|(_, r)| r.color(&params) == Some(Color::Black))
        .map(|(id, _)| *id)
        .expect("black moved");

    // White floods every group they own at black's ply.
    let mut flooded = state.clone();
    flooded.merge(&raw(&spam_moves(&w, &params, 2, 60)), &params);

    assert!(
        flooded.records.contains_key(&black_move),
        "grouping is per signer: only your own records compete with yours"
    );
}

#[test]
fn property_1_holds_after_eviction() {
    let (w, b, params) = keys();
    let mut a = GameState::empty();
    a.merge(&raw(&spam_moves(&w, &params, 1, 12)), &params);
    let mut peer_b = GameState::empty();
    peer_b.merge(&raw(&spam_moves(&b, &params, 2, 12)), &params);

    let delta = a.delta_against(&peer_b.summarize());
    let mut applied = peer_b.clone();
    applied.apply_delta(&delta, &params);

    // applyDelta(σ_B, δ) ⊔ σ_A == applyDelta(σ_B, δ)
    let joined = applied.merged(&a, &params);
    assert_eq!(applied.records, joined.records, "whitepaper Property 1");
}

#[test]
fn two_peers_converge_when_one_holds_a_record_the_other_evicts() {
    let (w, _b, params) = keys();
    let spam = spam_moves(&w, &params, 1, 20);
    let mut a = GameState::empty();
    a.merge(&raw(&spam[..10]), &params);
    let mut peer_b = GameState::empty();
    peer_b.merge(&raw(&spam[10..]), &params);

    // One bidirectional exchange.
    let to_b = a.delta_against(&peer_b.summarize());
    let to_a = peer_b.delta_against(&a.summarize());
    peer_b.apply_delta(&to_b, &params);
    a.apply_delta(&to_a, &params);

    assert_eq!(a.records, peer_b.records, "converged after one round trip");
}
```

Add to `common/tests/adversarial.rs`:

```rust
#[test]
fn flooding_draw_offers_cannot_evict_a_move_at_the_same_ply() {
    let (state, params, w, _b) = play(&["e2e4"]);
    let white_move = *state.records.keys().next().expect("one move");

    let offers: Vec<Record> = (0..50u64)
        .map(|i| {
            let mut at = [0u8; 32];
            at[..8].copy_from_slice(&i.to_le_bytes());
            Record::sign(&w, &params, Body::DrawOffer { ply: 1, at })
        })
        .collect();

    let mut flooded = state.clone();
    let mut raw_offers = GameState::empty();
    for o in &offers {
        raw_offers.absorb_for_test(o);
    }
    flooded.merge(&raw_offers, &params);

    assert!(
        flooded.records.contains_key(&white_move),
        "kind is part of the group key, so offers never compete with moves"
    );
}

/// The trade this design accepts, made executable so it cannot silently get
/// worse. Eviction must sort blind by id -- legality depends on the chain --
/// so a cheater can bury a double-sign under lower-id illegal records. The
/// forfeit is lost, but the result is a STALL, never a stolen win.
#[test]
fn a_buried_double_sign_stalls_instead_of_forfeiting() {
    let (state, params, w, _b) = play(&["e2e4", "e7e5"]);
    let head = project(&state, &params).chain.last().copied().expect("head");

    // Two genuinely different legal moves at ply 3: the fraud.
    let a = Record::sign(&w, &params, Body::Move { ply: 3, parent: head, uci: "g1f3".into() });
    let b = Record::sign(&w, &params, Body::Move { ply: 3, parent: head, uci: "b1c3".into() });

    let mut fraud = state.clone();
    fraud.merge(&raw_state(&[a.clone(), b.clone()]), &params);
    let caught = project(&fraud, &params);
    assert_eq!(
        caught.decision.map(|d| d.reason),
        Some(Reason::DoubleSignForfeit),
        "unburied, a double-sign still forfeits"
    );

    // Now bury both under lower-id records in the same group.
    let mut buried = state.clone();
    let mut junk: Vec<Record> = vec![a, b];
    for i in 0..64u64 {
        let mut parent = [0u8; 32];
        parent[..8].copy_from_slice(&i.to_le_bytes());
        junk.push(Record::sign(&w, &params, Body::Move { ply: 3, parent, uci: "e2e4".into() }));
    }
    buried.merge(&raw_state(&junk), &params);

    let stalled = project(&buried, &params);
    assert_eq!(stalled.decision, None, "the forfeit is evaded: no decision");
    assert_eq!(stalled.ply, 2, "and the chain stalls one ply short");
}
```

Add this helper to `common/tests/adversarial.rs`, beside `poison`:

```rust
/// A state holding exactly these records, with NO eviction applied.
fn raw_state(records: &[Record]) -> GameState {
    let mut s = GameState::empty();
    for r in records {
        s.absorb_for_test(r);
    }
    s
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p adjourn-core --locked`
Expected: compile failure — no method `evict` on `GameState`. After adding a stub, `eviction_bounds_a_spammed_group` fails with `50 != 2`.

- [ ] **Step 3: Implement eviction**

In `common/src/state.rs`, add the import and the group key above `impl GameState`:

```rust
use crate::types::{GameParams, Kind, KeyBytes, Record, RecordId};

/// One signer's records of one kind at one ply.
///
/// Per-signer grouping is what makes eviction safe against an opponent: your
/// records only ever compete with your own, so nobody else can evict your move,
/// and a player who spams themselves out of a legal move merely stalls their
/// own game.
type Group = (KeyBytes, Kind, u16);

fn group_of(rec: &Record) -> Option<Group> {
    Some((rec.signer, rec.body.kind(), rec.body.ply()?))
}
```

Add the method inside `impl GameState`:

```rust
    /// Keep only the K smallest ids in each `(signer, kind, ply)` group.
    ///
    /// This is what makes state bounded rather than merely small, and it keeps
    /// the monoid intact because top-K distributes over union:
    ///
    /// ```text
    /// topK(topK(A) ∪ topK(B)) = topK(A ∪ B)
    /// ```
    ///
    /// The K smallest ids of `A ∪ B` are necessarily present in
    /// `topK(A) ∪ topK(B)`, so filtering distributes and associativity,
    /// commutativity and idempotence all survive.
    ///
    /// Eviction sorts blind, by id. It CANNOT consider chess legality: legality
    /// is a function of the position, which is a function of the chain, which is
    /// a function of which records are present -- so a legality-aware rule would
    /// evict different records in a partial state and peers would diverge. The
    /// cost of sorting blind is that a cheater can bury a double-sign under
    /// lower-id illegal records; see the spec, and
    /// `a_buried_double_sign_stalls_instead_of_forfeiting`.
    pub fn evict(&mut self) {
        // BTreeMap iterates in id order, so each group's ids arrive ascending
        // and the first K are the K smallest.
        let mut groups: BTreeMap<Group, Vec<RecordId>> = BTreeMap::new();
        for (id, rec) in &self.records {
            if let Some(g) = group_of(rec) {
                groups.entry(g).or_default().push(*id);
            }
        }
        for ((_, kind, _), ids) in groups {
            let k = kind.k();
            if ids.len() > k {
                for id in &ids[k..] {
                    self.records.remove(id);
                }
            }
        }
    }
```

- [ ] **Step 4: Apply eviction on every write path**

Still in `common/src/state.rs`, change three methods. `merge` evicts once after the union rather than per-insert — one `O(n)` pass instead of `O(n²)`:

```rust
    pub fn merge(&mut self, other: &GameState, params: &GameParams) {
        for rec in other.records.values() {
            self.absorb(rec, params);
        }
        self.evict();
    }
```

```rust
    /// Admit a single record from an untrusted source.
    pub fn insert_verified(&mut self, rec: &Record, params: &GameParams) -> bool {
        self.absorb(rec, params);
        self.evict();
        rec.verify(params)
    }
```

```rust
    pub fn apply_delta(&mut self, delta: &Delta, params: &GameParams) {
        for rec in delta {
            self.absorb(rec, params);
        }
        self.evict();
    }
```

`filter_valid` already merges into an empty state, so it evicts through `merge` with no change. Leave `all_valid` alone: it must stay permissive.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p adjourn-core --locked`
Expected: PASS. The randomized law tests take ~100s; that is normal.

- [ ] **Step 6: Add the contract-level normalization test**

This must go through the **real interface**, not just call `merge` — the point
is that a peer cannot push an over-K state onto the network by handing the
contract crafted bytes. `interface.rs` already has `keys()`, `encode()`,
`state_bytes()`, `params_bytes()` and the `update()` helper; use them.

```rust
#[test]
fn an_over_k_state_comes_back_normalized_through_update() {
    let (w, _b, params) = keys();

    // Crafted bytes: 30 records from one signer in one (signer, kind, ply)
    // group. `absorb_for_test` bypasses eviction, which is exactly what a
    // hostile peer's encoder would do.
    let mut spam = GameState::empty();
    for i in 0..30u64 {
        let mut parent = [0u8; 32];
        parent[..8].copy_from_slice(&i.to_le_bytes());
        spam.absorb_for_test(&Record::sign(
            &w,
            &params,
            Body::Move { ply: 1, parent, uci: "e2e4".into() },
        ));
    }
    assert_eq!(spam.len(), 30, "the crafted state really is over-K");

    let out = update(
        &params,
        &GameState::empty(),
        vec![UpdateData::State(state_bytes(&spam))],
    )
    .expect("update_state");

    assert_eq!(out.len(), 2, "the contract normalizes an over-K state to K=2");
}
```

- [ ] **Step 7: Run the whole suite**

Run: `cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add common/src/state.rs common/tests/ contracts/adjourn-contract/tests/interface.rs
git commit -m "feat(core)!: top-K eviction bounds state at merge"
```

---

### Task 3: `DrawClaim` projection, precedence, and `Status.ignored`

**Files:**
- Modify: `common/src/project.rs`
- Test: `common/tests/adversarial.rs`

**Interfaces:**
- Consumes: `Body::DrawClaim { ply, at }` from Task 1.
- Produces: `Reason::ThreefoldClaim`, `Reason::FiftyMoveClaim`; `Status.ignored` redefined as the count of `Move` records not in the chain.

- [ ] **Step 1: Write the failing tests**

Add to `common/tests/adversarial.rs`. The repetition line shuffles knights back and forth, which repeats the start position:

```rust
/// Threefold needs the SAME position three times. Knights out and back does it.
const THREEFOLD_LINE: &[&str] = &[
    "g1f3", "g8f6", "f3g1", "f6g8", // position 2
    "g1f3", "g8f6", "f3g1", "f6g8", // position 3
];

#[test]
fn a_threefold_claim_at_the_head_draws_the_game() {
    let (state, params, w, _b) = play(THREEFOLD_LINE);
    let status = project(&state, &params);
    assert!(status.repetitions >= 3, "the line must actually repeat");
    assert!(status.decision.is_none(), "threefold alone does not end the game");

    // White is to move at the head, so White is the one who may claim.
    assert_eq!(status.turn, Color::White, "this line ends with white to move");
    let head = status.chain.last().copied().expect("head");
    let claim = Record::sign(&w, &params, Body::DrawClaim { ply: status.ply, at: head });

    let mut claimed = state.clone();
    assert!(claimed.insert_verified(&claim, &params));
    let after = project(&claimed, &params);
    assert_eq!(
        after.decision.map(|d| d.reason),
        Some(Reason::ThreefoldClaim),
        "a valid claim at the head draws"
    );
    assert_eq!(after.decision.and_then(|d| d.winner), None, "a claim is a draw");
}

#[test]
fn a_claim_with_no_valid_ground_is_ignored_not_fatal() {
    let (state, params, w, _b) = play(&["e2e4", "e7e5"]);
    let status = project(&state, &params);
    let head = status.chain.last().copied().expect("head");
    let claim = Record::sign(&w, &params, Body::DrawClaim { ply: status.ply, at: head });

    let mut claimed = state.clone();
    assert!(claimed.insert_verified(&claim, &params));
    let after = project(&claimed, &params);
    assert_eq!(after.decision, None, "no repetition, no fifty-move: ignored");
    assert_eq!(after.ply, status.ply, "and the game is otherwise untouched");
}

#[test]
fn a_stale_claim_is_ignored() {
    let (state, params, w, _b) = play(THREEFOLD_LINE);
    let status = project(&state, &params);
    let head = status.chain.last().copied().expect("head");
    let claim = Record::sign(&w, &params, Body::DrawClaim { ply: status.ply, at: head });

    // The claimant moves instead of standing on the claim, advancing the head.
    // Only the claimant can do this -- which is precisely why a claim has no
    // race: the opponent cannot void it.
    let mut moved = state.clone();
    let mv = make_move(&moved, &params, &w, "e2e4").expect("legal");
    assert!(moved.insert_verified(&mv, &params));
    assert!(moved.insert_verified(&claim, &params));

    assert_eq!(
        project(&moved, &params).decision,
        None,
        "the head moved on, so the claim expired"
    );
}

#[test]
fn a_valid_claim_outranks_a_pending_draw_agreement() {
    let (state, params, w, b) = play(THREEFOLD_LINE);
    let status = project(&state, &params);
    let head = status.chain.last().copied().expect("head");

    // Black offers, white accepts -- a complete, live agreement.
    let offer = Record::sign(&b, &params, Body::DrawOffer { ply: status.ply, at: head });
    let accept = Record::sign(&w, &params, Body::DrawAccept { ply: status.ply, offer: offer.id() });
    // White, who is to move, also claims the threefold.
    let claim = Record::sign(&w, &params, Body::DrawClaim { ply: status.ply, at: head });

    let mut both = state.clone();
    for rec in [&offer, &accept, &claim] {
        assert!(both.insert_verified(rec, &params));
    }

    // Both are draws, so the winner is the same either way -- what the
    // precedence decides is which REASON a peer reports. Every peer must
    // report the same one.
    assert_eq!(
        project(&both, &params).decision.map(|d| d.reason),
        Some(Reason::ThreefoldClaim),
        "claim sits above agreement in the precedence"
    );
}

#[test]
fn only_the_player_to_move_may_claim() {
    let (state, params, _w, b) = play(THREEFOLD_LINE);
    let status = project(&state, &params);
    assert_eq!(status.turn, Color::White, "white is to move on this line");
    let head = status.chain.last().copied().expect("head");

    // Black is NOT to move, so black's claim must not count -- otherwise the
    // player to move could void it by moving, reintroducing a race.
    let claim = Record::sign(&b, &params, Body::DrawClaim { ply: status.ply, at: head });
    let mut claimed = state.clone();
    assert!(claimed.insert_verified(&claim, &params));
    assert_eq!(project(&claimed, &params).decision, None);
}

/// Note on what this does and does not prove. A position that is BOTH
/// checkmate and threefold/fifty-move is impractical to construct by hand, so
/// this asserts the weaker, still-useful property: a claim record at a
/// checkmate head does not disturb the mate. The strict board > claim ordering
/// is structural — `board_result.or_else(...)` short-circuits before
/// `draw_claimed` is ever called — and is documented at that call site.
#[test]
fn a_claim_does_not_disturb_a_checkmate() {
    let (state, params, _w, b) = play(SCHOLARS);
    let status = project(&state, &params);
    assert_eq!(
        status.decision.map(|d| d.reason),
        Some(Reason::Checkmate),
        "scholar's mate ends in checkmate"
    );
    let head = status.chain.last().copied().expect("head");

    // Black is the mated player, and therefore the player "to move".
    let claim = Record::sign(&b, &params, Body::DrawClaim { ply: status.ply, at: head });
    let mut claimed = state.clone();
    claimed.insert_verified(&claim, &params);

    assert_eq!(
        project(&claimed, &params).decision.map(|d| d.reason),
        Some(Reason::Checkmate),
        "a mated player cannot claim their way out of a loss"
    );
}

#[test]
fn ignored_counts_illegal_moves_but_not_resignations() {
    let (state, params, w, b) = play(&["e2e4", "e7e5"]);
    let base = project(&state, &params).ignored;
    assert_eq!(base, 0, "a clean game ignores nothing");

    // A resignation is a statement, not an ignored move.
    let mut with_resign = state.clone();
    assert!(with_resign.insert_verified(&Record::sign(&b, &params, Body::Resign), &params));
    assert_eq!(
        project(&with_resign, &params).ignored,
        0,
        "resignations are not ignored moves"
    );

    // A wrong-parent move IS an ignored move.
    let mut with_junk = state.clone();
    assert!(with_junk.insert_verified(
        &Record::sign(&w, &params, Body::Move { ply: 3, parent: [9u8; 32], uci: "g1f3".into() }),
        &params
    ));
    assert_eq!(project(&with_junk, &params).ignored, 1);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p adjourn-core --locked`
Expected: compile failure — no variant `Reason::ThreefoldClaim`.

- [ ] **Step 3: Add the claim reasons and the claim thresholds**

In `common/src/project.rs`, beside the existing `FIVEFOLD` constant:

```rust
/// FIDE 9.2: a player may CLAIM a draw when a position occurs a third time.
const THREEFOLD: u32 = 3;

/// FIDE 9.3: a player may CLAIM a draw after 50 moves by each player with no
/// capture and no pawn move -- 100 halfmoves.
const FIFTY_MOVE_HALFMOVES: u32 = 100;
```

Add to `Reason`:

```rust
    /// A claimed threefold repetition (FIDE 9.2).
    ThreefoldClaim,
    /// A claimed fifty-move draw (FIDE 9.3).
    FiftyMoveClaim,
```

- [ ] **Step 4: Add the claim resolver**

In `common/src/project.rs`, after `draw_agreed`:

```rust
/// Is there a live, well-founded draw claim at the head?
///
/// Anchored to the head exactly like `DrawOffer`, which is what keeps it free
/// of races: the claimant must be the player to move, and the player to move is
/// the only party who can advance the head. The opponent therefore cannot void
/// a valid claim, and a claim withheld and published later simply does nothing.
///
/// A claim with no valid ground is ignored, never fatal -- invariant 1 applies
/// to claims exactly as it applies to illegal moves.
fn draw_claimed(
    state: &GameState,
    params: &GameParams,
    head: RecordId,
    turn: Color,
    repetitions: u32,
    halfmoves: u32,
) -> Option<Reason> {
    for rec in state.records.values() {
        let Body::DrawClaim { at, .. } = &rec.body else {
            continue;
        };
        if *at != head {
            continue; // the game moved on: this claim has expired
        }
        // Only the player to move may claim. Letting the idle player claim
        // would let the player to move void it by moving -- a race.
        if rec.color(params) != Some(turn) {
            continue;
        }
        if repetitions >= THREEFOLD {
            return Some(Reason::ThreefoldClaim);
        }
        if halfmoves >= FIFTY_MOVE_HALFMOVES {
            return Some(Reason::FiftyMoveClaim);
        }
    }
    None
}
```

- [ ] **Step 5: Slot the claim into the precedence**

In `project`, replace the `(false, false)` arm so the order is
`forfeit > resignation > board > claim > agreement`:

```rust
            (false, false) => board_result.or_else(|| {
                let head = chain.last().copied().unwrap_or_else(|| params.genesis());
                // Board first: the claimant is by definition the player to
                // move, so if that position is checkmate the claimant is the
                // player who has been mated. Ranking the claim above the board
                // would let a mated player draw their way out of a loss.
                draw_claimed(
                    state,
                    params,
                    head,
                    pos.turn(),
                    repetitions,
                    pos.halfmoves(),
                )
                .map(|reason| Decision { winner: None, reason })
                .or_else(|| {
                    if draw_agreed(state, params, head) {
                        Some(Decision {
                            winner: None,
                            reason: Reason::DrawAgreement,
                        })
                    } else {
                        None
                    }
                })
            }),
```

- [ ] **Step 6: Fix `Status.ignored`**

In `project`, replace the `ignored` field. It currently reads
`state.len().saturating_sub(chain.len())`, which counts resignations and draw
records as ignored moves:

```rust
    // Only MOVE records can be "ignored" -- illegal moves, wrong-parent moves,
    // and moves past the point the chain stopped. A resignation or a draw
    // record is a statement in its own right, not a move the projection
    // skipped.
    let in_chain: std::collections::BTreeSet<RecordId> = chain.iter().copied().collect();
    let ignored = state
        .records
        .iter()
        .filter(|(id, rec)| matches!(rec.body, Body::Move { .. }) && !in_chain.contains(*id))
        .count();
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add common/src/project.rs common/tests/adversarial.rs
git commit -m "feat(core): DrawClaim projection, claim precedence, precise Status.ignored"
```

---

### Task 4: CLI `draw claim`

**Files:**
- Modify: `cli/src/session.rs`, `cli/src/main.rs`
- Test: `cli/tests/moves.rs`

**Interfaces:**
- Consumes: `Body::DrawClaim { ply, at }`, `Reason::ThreefoldClaim`, `Reason::FiftyMoveClaim`.
- Produces: `session::draw_claim(node, label, contract_wasm) -> anyhow::Result<Status>`.

> **Scope note for the executor:** the spec lists the CLI as out of scope. This
> task exists because without it `DrawClaim` is unreachable — the record type
> would ship with no way to produce one. Build it; if the controller rules it
> out, drop the task rather than expanding it.

- [ ] **Step 1: Write the failing test**

In `cli/tests/moves.rs`. The file already has an `async fn setup() ->
Option<(FakeNode, FakeNode, Vec<u8>)>` helper that builds two bound `FakeNode`s;
it returns `None` when the contract WASM is not on disk, and every test in the
file skips on that with the `else { return eprintln!(...) }` form. Follow that
shape exactly — do not invent a new helper:

```rust
/// A groundless claim is refused locally. A claim with no valid ground is
/// ignored at projection anyway, so signing one would only add a dead record
/// to state.
#[tokio::test]
async fn draw_claim_refuses_when_there_is_no_ground_to_claim() {
    let Some((mut alice, _bob, wasm)) = setup().await else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };

    let err = draw_claim(&mut alice, "alice", wasm)
        .await
        .expect_err("a fresh game has nothing to claim");
    let text = format!("{err:#}").to_lowercase();
    assert!(text.contains("no draw to claim"), "got: {err:#}");
}
```

Add `draw_claim` to the existing `use adjourn_cli::session::{...}` import list
at the top of the file.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p adjourn-cli --locked draw_claim`
Expected: compile failure — no function `draw_claim` in `session`.

- [ ] **Step 3: Implement the flow**

In `cli/src/session.rs`, after `draw_accept`:

```rust
/// Claim a draw by threefold repetition or the fifty-move rule.
///
/// Both are FIDE *claims* (9.2, 9.3), not automatic results, so nothing happens
/// until a player asks. Checked locally first: a claim with no ground is ignored
/// at projection anyway, so signing one would just add a dead record to state.
pub async fn draw_claim<N: NodeClient>(
    node: &mut N,
    label: &str,
    contract_wasm: Vec<u8>,
) -> anyhow::Result<Status> {
    let g = open_game(node, label, contract_wasm).await?;
    if g.status.is_over() {
        bail!("the game is already over");
    }
    let our_color: Color = g.side.into();
    if g.status.turn != our_color {
        bail!("only the player to move may claim a draw");
    }
    if g.status.repetitions < 3 && g.status.halfmove_clock < 100 {
        bail!(
            "no draw to claim: {} repetitions, {} halfmoves since a capture or pawn move",
            g.status.repetitions,
            g.status.halfmove_clock
        );
    }
    let at = g
        .status
        .chain
        .last()
        .copied()
        .unwrap_or_else(|| g.params.genesis());

    sign_and_submit(
        node,
        g.game_id,
        g.container,
        &g.params,
        g.contract,
        Body::DrawClaim { ply: g.status.ply, at },
    )
    .await
}
```

- [ ] **Step 4: Wire up the subcommand**

In `cli/src/main.rs`, add a third variant to `DrawCommand` (the enum at line 130,
beside `Offer` at 132 and `Accept` at 137). Copy the `--label` argument
attributes verbatim from the `Accept` variant directly above it:

```rust
    /// Claim a draw by threefold repetition or the fifty-move rule.
    Claim {
        #[arg(long)]
        label: String,
    },
```

Then add the dispatch arm immediately after the `DrawCommand::Accept` arm at
line 322, mirroring its shape exactly — same client construction, same
contract-WASM load, same renderer for the returned `Status`:

Copy the body of the `DrawCommand::Accept` arm verbatim and change only the
session call. Note that `output::render_status` takes **two** arguments —
`(label: &str, status: &Status)` — so the label is passed through:

```rust
        Command::Draw(DrawCommand::Claim { label }) => {
            let status = session::draw_claim(&mut client, &label, contract_wasm()?).await?;
            output::render_status(&label, &status);
        }
```

If the surrounding arms name things differently (a different client binding, a
different WASM loader), follow **their** names rather than these — the point is
that `Claim` is indistinguishable in shape from `Accept`.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p adjourn-cli --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add cli/src/session.rs cli/src/main.rs cli/tests/moves.rs
git commit -m "feat(cli): draw claim command"
```

---

### Task 5: Documentation

**Files:**
- Modify: `CLAUDE.md`

**Interfaces:**
- Consumes: everything above. Produces: nothing.

- [ ] **Step 1: Update the wire format table**

In the "Wire format" section, add the three changed/new rows:

```markdown
| `DrawOffer.ply` / `.at` | `p` / `t` |
| `DrawAccept.ply` / `.offer` | `p` / `o` |
| `Body::DrawClaim` | `c` |
| `DrawClaim.ply` / `.at` | `p` / `t` |
```

- [ ] **Step 2: Amend invariant 7**

Change the precedence line to
`forfeit > **resignation** > board result > **draw claim** > draw agreement`,
and add this paragraph after the existing resignation rationale:

```markdown
   The board result outranks a draw claim because the claimant is by definition
   the player to move — so if that position is checkmate, the claimant is the
   player who has just been mated. Ranking the claim first would let a mated
   player draw their way out of a loss. Test:
   `the_board_result_outranks_a_claim`.
```

- [ ] **Step 3: Add an invariant for the eviction group key**

Add as invariant 10:

```markdown
10. **The eviction group key includes the record's `kind`.**
    Merge keeps only the K smallest ids per `(signer, kind, ply)` group. Were
    kind left out, a player could flood `DrawOffer` records at ply N and evict
    their own `Move` records at ply N — including both halves of a double-sign
    fraud proof. Grouping per *signer* is what makes eviction safe against an
    opponent: your records only ever compete with your own. Tests:
    `flooding_draw_offers_cannot_evict_a_move_at_the_same_ply`,
    `a_spammer_cannot_evict_the_opponents_records`.
```

- [ ] **Step 4: Rewrite the unbounded-growth known issue**

Replace the whole "Unbounded state growth" entry. The existing claim that
"K=2 preserves the double-sign fraud proof" is **false** under this design and
must not survive:

```markdown
- **State growth: bounded, at a cost.** Merge keeps the K smallest ids per
  `(signer, kind, ply)` group — K=2 for moves, K=1 for draw records — and
  `MAX_PLY = 4096` bounds the number of groups. Worst case is ~41,000 records
  or ~6.4 MB, against a normal game's 1100 bytes. Top-K distributes over merge
  (`topK(topK(A) ∪ topK(B)) = topK(A ∪ B)`), so the rule is idempotent and the
  monoid survives.

  A ply-window rule ("drop moves beyond chain length + 1") is still **not**
  safe: chain length is shorter in a partial state, so a peer would evict
  records the merged state needs.

- **The double-sign forfeit is evadable.** Eviction must sort blind by id:
  legality depends on the position, which depends on the chain, which depends
  on which records are present, so a legality-aware rule would evict different
  records in a partial state and peers would diverge. A cheater can therefore
  bury a double-sign under lower-id *illegal* records at the same ply — `walk`
  finds no legal candidates, the chain stops one ply short, and no forfeit
  fires. Any fixed K is beatable this way.

  The cheater gains nothing new: the outcome is a stalled game with no
  decision, which is what walking away already produces. No win is ever
  stolen — it converts a loss into a no-result. A witness-published fraud
  proof embedding both offending records would fix it, but defining fraud
  without a position collides with invariant 8's castling case. Test:
  `a_buried_double_sign_stalls_instead_of_forfeiting`.
```

- [ ] **Step 5: Close the resolved known issues**

Delete the **`ply: u16` overflows** entry — `MAX_PLY` makes the overflow
unreachable. Delete the **`Status.ignored` is imprecise** entry — it now counts
only `Move` records outside the chain.

Replace the **threefold and fifty-move** entry with this. The old text ends
"Adding a `DrawClaim` body is a live design question", which is no longer true:

```markdown
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
```

Also update the **75-move threshold has no end-to-end test** entry only if the
work above happened to add one. It did not, so leave that entry as it stands —
do not claim coverage that does not exist.

- [ ] **Step 6: Update the test counts**

Run `cargo test --workspace --locked`, count the per-file results, and update
both the summary line in the "Testing" section and the per-file bullets beneath
it. Do not guess: use the numbers the run prints.

- [ ] **Step 7: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: record the eviction trade, the claim tier, and MAX_PLY"
```

---

## Notes for the executor

- **`merge` verifies signatures**, so the randomized law tests take ~100s. That
  is expected, not a hang.
- **The delegate crate cannot be host-compiled on Windows** (`freenet-stdlib` →
  `tracing-subscriber` → `windows-sys`, which needs a full mingw binutils).
  Verify it with `cargo check -p adjourn-delegate --target wasm32-unknown-unknown`.
  The CLI is the opposite case: it cannot be built for wasm32, because
  `tokio-tungstenite` → `mio` has no wasm32 backend. Check the CLI natively.
- **Do not add `rand` or `getrandom`** to any crate in the contract or delegate
  dependency graph. CI asserts the graph stays clean.
