//! The contract state and its algebra.
//!
//! State is an unordered set of signed records. Merge is set union with a
//! deterministic tiebreak. That is the entire consistency story — associative,
//! commutative and idempotent by construction, with no chess knowledge in it
//! at all. Ordering comes from the parent-hash chain, resolved at projection
//! time (see `project.rs`).

use crate::types::{GameParams, Record, RecordId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameState {
    /// BTreeMap, not HashMap: canonical iteration order gives byte-identical
    /// serialization on every peer holding the same logical state.
    pub records: BTreeMap<RecordId, Record>,
}

/// Compact representation of what a peer holds. Exact, not probabilistic —
/// no Bloom filter false positives to design a second sync round around.
///
/// Maps each record id to a digest of the signature the peer is holding for
/// it. The id alone is NOT enough: ids exclude the signature, so two peers can
/// hold different bytes under the same id and a set-of-ids summary would report
/// them as already in sync. Carrying the digest is what keeps whitepaper
/// Property 1 (sync soundness) true in the collision case.
pub type Summary = BTreeMap<RecordId, SigDigest>;

/// SHA-256 of a record's signature bytes; see [`Record::sig_digest`].
pub type SigDigest = [u8; 32];

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
    }

    pub fn merged(&self, other: &GameState, params: &GameParams) -> GameState {
        let mut out = self.clone();
        out.merge(other, params);
        out
    }

    /// Admit a single record from an untrusted source.
    pub fn insert_verified(&mut self, rec: &Record, params: &GameParams) -> bool {
        self.absorb(rec, params);
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
            self.insert_verified(rec, params);
        }
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
