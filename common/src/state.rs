//! The contract state and its algebra.
//!
//! State is an unordered set of signed records. Merge is set union with a
//! deterministic tiebreak. That is the entire consistency story — associative,
//! commutative and idempotent by construction, with no chess knowledge in it
//! at all. Ordering comes from the parent-hash chain, resolved at projection
//! time (see `project.rs`).

use crate::types::{GameParams, Kind, KeyBytes, Record, RecordId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GameState {
    /// BTreeMap, not HashMap: canonical iteration order gives byte-identical
    /// serialization on every peer holding the same logical state.
    pub records: BTreeMap<RecordId, Record>,
}

/// On the wire a state is a bare sequence of records, NOT a map.
///
/// The map key is `rec.id()`, which is derived from the record itself, so
/// sending it costs 34 bytes a record to transmit something the receiver can
/// compute. `BTreeMap` iteration is in id order, so the sequence is still
/// canonical — invariant 5 is about byte-identical output across peers, and
/// that holds.
impl Serialize for GameState {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.records.values())
    }
}

impl<'de> Deserialize<'de> for GameState {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let records = Vec::<Record>::deserialize(d)?;
        let mut out = BTreeMap::new();
        for rec in records {
            // An honestly-serialized state cannot repeat an id: map keys are
            // unique. Duplicates therefore mean crafted bytes — and decoding
            // has no `params`, so it cannot tell an honest signature from a
            // forgery. Refuse rather than silently pick a winner.
            if out.insert(rec.id(), rec).is_some() {
                return Err(serde::de::Error::custom(
                    "two records share one id; state is malformed",
                ));
            }
        }
        Ok(GameState { records: out })
    }
}

/// SHA-256 of a record's signature bytes; see [`Record::sig_digest`].
pub type SigDigest = [u8; 32];

/// Compact representation of what a peer holds. Exact, not probabilistic —
/// no Bloom filter false positives to design a second sync round around.
///
/// Maps each record id to a digest of the signature the peer is holding for
/// it. The id alone is NOT enough: ids exclude the signature, so two peers can
/// hold different bytes under the same id and a set-of-ids summary would report
/// them as already in sync. Carrying the digest is what keeps whitepaper
/// Property 1 (sync soundness) true in the collision case.
///
/// A newtype rather than a `BTreeMap` alias so it can carry its own encoding:
/// see the `Serialize` impl. Derefs to the map, so reads are unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary(BTreeMap<RecordId, SigDigest>);

impl Summary {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, id: RecordId, digest: SigDigest) -> Option<SigDigest> {
        self.0.insert(id, digest)
    }
}

impl std::ops::Deref for Summary {
    type Target = BTreeMap<RecordId, SigDigest>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FromIterator<(RecordId, SigDigest)> for Summary {
    fn from_iter<I: IntoIterator<Item = (RecordId, SigDigest)>>(iter: I) -> Self {
        Summary(iter.into_iter().collect())
    }
}

/// The summary rides on EVERY sync round, so its encoding matters more than
/// the state's. As a plain map of two `[u8; 32]`s, serde emits both halves as
/// CBOR arrays of integers — about 110 bytes to carry 64 bytes of content.
///
/// Packed as one byte string of `id ‖ digest` entries in id order it is
/// exactly 64 bytes an entry, and still canonical: `BTreeMap` iterates sorted.
impl Serialize for Summary {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut buf = Vec::with_capacity(self.0.len() * 64);
        for (id, digest) in &self.0 {
            buf.extend_from_slice(id);
            buf.extend_from_slice(digest);
        }
        s.serialize_bytes(&buf)
    }
}

impl<'de> Deserialize<'de> for Summary {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let buf = serde_bytes::ByteBuf::deserialize(d)?;
        let buf: &[u8] = buf.as_ref();
        if !buf.len().is_multiple_of(64) {
            return Err(serde::de::Error::custom(
                "summary is not a whole number of 64-byte entries",
            ));
        }
        let mut out = BTreeMap::new();
        for chunk in buf.chunks_exact(64) {
            let id: RecordId = chunk[..32].try_into().expect("32 bytes");
            let digest: SigDigest = chunk[32..].try_into().expect("32 bytes");
            if out.insert(id, digest).is_some() {
                return Err(serde::de::Error::custom("summary repeats a record id"));
            }
        }
        Ok(Summary(out))
    }
}

/// The records the other peer is missing.
pub type Delta = Vec<Record>;

impl GameState {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Insert one record, keeping the lexicographically smaller signature on
    /// collision.
    ///
    /// Ids exclude the signature, so two records can share an id with
    /// different signature bytes. Picking `min` rather than "first writer
    /// wins" is what keeps merge commutative in that case.
    ///
    /// The tiebreak runs ONLY between records that verify. Comparing raw bytes
    /// without regard to validity would let an all-zero signature — minimal,
    /// and forgeable by anyone who has seen the record — evict the honest one.
    /// Verifying here is what makes merge and `filter_valid` commute.
    fn absorb(&mut self, rec: &Record, params: &GameParams) {
        let id = rec.id();
        // Re-absorbing a record we already hold byte-for-byte is a no-op, so
        // skip the signature check. This is the common case during sync, and
        // ed25519 verification is by far the most expensive thing here.
        if self
            .records
            .get(&id)
            .is_some_and(|held| held.sig == rec.sig)
        {
            return;
        }
        if !rec.verify(params) {
            return;
        }
        match self.records.get(&id) {
            // A verified record always displaces an unverified one; between
            // two verified records the smaller signature wins.
            Some(existing) if existing.verify(params) && existing.sig <= rec.sig => {}
            _ => {
                self.records.insert(id, rec.clone());
            }
        }
    }

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
    /// evict different records in a partial state and peers would diverge.
    ///
    /// Sorting blind means a cheater can bury a *legality-based* fraud proof
    /// under lower-id junk. That is why the double-sign proof is structural
    /// instead (`project::double_signed`): it counts `Move` records per
    /// `(signer, ply)`, and K=2 FLOORS this group rather than emptying it, so
    /// burial cannot dissolve the proof -- the junk used to bury it is itself
    /// two records in one group. This is what makes K=2 load-bearing.
    /// Test: `a_buried_double_sign_still_forfeits`.
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

    /// The monoid operation. Associative, commutative, idempotent, with the
    /// empty state as identity.
    ///
    /// `params` is not state — it is the contract's fixed parameters. The
    /// monoid is over the set of *valid* records, so absorption has to be able
    /// to tell which those are.
    pub fn merge(&mut self, other: &GameState, params: &GameParams) {
        for rec in other.records.values() {
            self.absorb(rec, params);
        }
        self.evict();
    }

    pub fn merged(&self, other: &GameState, params: &GameParams) -> GameState {
        let mut out = self.clone();
        out.merge(other, params);
        out
    }

    /// Admit a single record from an untrusted source.
    pub fn insert_verified(&mut self, rec: &Record, params: &GameParams) -> bool {
        self.absorb(rec, params);
        self.evict();
        rec.verify(params)
    }

    /// Drop anything not signed by one of the two players. Applied at the
    /// contract's validity boundary so forged records never enter state.
    pub fn filter_valid(&self, params: &GameParams) -> GameState {
        let mut out = GameState::empty();
        out.merge(self, params);
        out
    }

    pub fn all_valid(&self, params: &GameParams) -> bool {
        self.records.values().all(|r| r.verify(params))
    }

    pub fn summarize(&self) -> Summary {
        self.records
            .iter()
            .map(|(id, rec)| (*id, rec.sig_digest()))
            .collect()
    }

    /// The records the other peer is missing, or holds under a different
    /// signature.
    ///
    /// Offering on a digest mismatch rather than only on a missing id is what
    /// closes the collision case: both peers offer, both absorb, and both land
    /// on the same min(sig) winner in a single round.
    pub fn delta_against(&self, summary: &Summary) -> Delta {
        self.records
            .iter()
            .filter(|(id, rec)| summary.get(*id) != Some(&rec.sig_digest()))
            .map(|(_, rec)| rec.clone())
            .collect()
    }

    pub fn apply_delta(&mut self, delta: &Delta, params: &GameParams) {
        for rec in delta {
            self.absorb(rec, params);
        }
        self.evict();
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        ciborium::into_writer(self, &mut buf).expect("cbor encode");
        buf
    }

    pub fn decode(bytes: &[u8]) -> Option<GameState> {
        ciborium::from_reader(bytes).ok()
    }
}

impl GameState {
    /// Inserts without verifying, so tests can build the malformed states an
    /// attacker would put on the wire. Not reachable through the normal API.
    pub fn absorb_for_test(&mut self, rec: &Record) {
        self.records.insert(rec.id(), rec.clone());
    }
}
