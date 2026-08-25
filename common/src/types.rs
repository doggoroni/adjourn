//! Signed records and game parameters.
//!
//! Everything a player can say about a game is a `Record`: a `Body` plus the
//! signer's ed25519 public key plus a signature. Records are content-addressed
//! by `(signer, body)` — deliberately NOT including the signature, so that two
//! encodings of the same statement collapse to one entry under merge.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use shakmaty::Color;

pub type RecordId = [u8; 32];
pub type KeyBytes = [u8; 32];

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
            // Two, so the structural double-sign proof survives eviction.
            // This is the load-bearing choice, not a decorative one: a group
            // of two is FLOORED at two rather than emptied, so a cheater
            // cannot spam their own group down to a single clean record and
            // erase the evidence. See `project::double_signed`.
            Kind::Move => 2,
            // At a given ply there is exactly one head, so exactly one
            // legitimate `at`. K=1 costs an honest player nothing.
            _ => 1,
        }
    }
}

const DOMAIN_GAME: &[u8] = b"adjourn-v1/game";
const DOMAIN_GENESIS: &[u8] = b"adjourn-v1/genesis";
const DOMAIN_REC: &[u8] = b"adjourn-v1/rec";
const DOMAIN_SIG: &[u8] = b"adjourn-v1/sig";
const DOMAIN_SIGDIGEST: &[u8] = b"adjourn-v1/sigdigest";

/// Immutable contract parameters, fixed at creation and folded into the
/// contract key. Both players can derive the same key independently.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameParams {
    #[serde(with = "serde_bytes")]
    pub white: KeyBytes,
    #[serde(with = "serde_bytes")]
    pub black: KeyBytes,
    /// Distinguishes repeat matchups between the same two players.
    #[serde(with = "serde_bytes")]
    pub nonce: [u8; 16],
}

impl GameParams {
    /// Binds every signature to this specific game, preventing a move signed in
    /// one game from being replayed into another.
    pub fn game_id(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(DOMAIN_GAME);
        h.update(self.white);
        h.update(self.black);
        h.update(self.nonce);
        h.finalize().into()
    }

    /// The synthetic parent of ply 1. Anchors the chain to this game.
    pub fn genesis(&self) -> RecordId {
        let mut h = Sha256::new();
        h.update(DOMAIN_GENESIS);
        h.update(self.game_id());
        h.finalize().into()
    }

    pub fn color_of(&self, key: &KeyBytes) -> Option<Color> {
        if key == &self.white {
            Some(Color::White)
        } else if key == &self.black {
            Some(Color::Black)
        } else {
            None
        }
    }

    pub fn key_of(&self, color: Color) -> KeyBytes {
        match color {
            Color::White => self.white,
            Color::Black => self.black,
        }
    }
}

/// Whose turn it is at a given ply. Ply is 1-indexed in half-moves.
pub fn color_at_ply(ply: u16) -> Color {
    if ply % 2 == 1 {
        Color::White
    } else {
        Color::Black
    }
}

/// The four things a player can assert. All are monotone: once said, a
/// statement is never retracted, only accumulated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Body {
    /// A half-move. `parent` is the id of the ply-1 record (or genesis at ply 1).
    #[serde(rename = "m")]
    Move {
        #[serde(rename = "p")]
        ply: u16,
        #[serde(rename = "t", with = "serde_bytes")]
        parent: RecordId,
        #[serde(rename = "u")]
        uci: String,
    },
    /// Unconditional. Position-independent, so it needs no anchor.
    #[serde(rename = "r")]
    Resign,
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
}

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    #[serde(rename = "b")]
    pub body: Body,
    #[serde(rename = "k", with = "serde_bytes")]
    pub signer: KeyBytes,
    /// 64 raw ed25519 bytes.
    #[serde(rename = "s", with = "serde_bytes")]
    pub sig: Vec<u8>,
}

fn body_bytes(body: &Body) -> Vec<u8> {
    let mut buf = Vec::new();
    ciborium::into_writer(body, &mut buf).expect("cbor encode");
    buf
}

/// What gets signed. Domain-separated and bound to the game id.
pub fn signing_payload(game_id: &[u8; 32], body: &Body) -> Vec<u8> {
    let mut buf = Vec::with_capacity(96);
    buf.extend_from_slice(DOMAIN_SIG);
    buf.extend_from_slice(game_id);
    buf.extend_from_slice(&body_bytes(body));
    buf
}

impl Record {
    /// Content address of the statement, excluding the signature.
    ///
    /// Excluding the signature is what makes merge idempotent in the presence
    /// of signature malleability: the same statement always lands in the same
    /// slot regardless of which signing implementation produced it.
    pub fn id(&self) -> RecordId {
        let mut h = Sha256::new();
        h.update(DOMAIN_REC);
        h.update(self.signer);
        h.update(body_bytes(&self.body));
        h.finalize().into()
    }

    /// Digest of the signature bytes.
    ///
    /// The id deliberately excludes the signature (so malleability cannot split
    /// one statement across two slots), which means the id alone does not
    /// determine a record's bytes: ed25519 does not pin the nonce, so a player
    /// can emit two *valid* signatures over one body. The summary publishes
    /// this digest alongside the id so that sync can still see the difference
    /// and converge on the min(sig) winner.
    pub fn sig_digest(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(DOMAIN_SIGDIGEST);
        h.update(&self.sig);
        h.finalize().into()
    }

    pub fn sign(key: &SigningKey, params: &GameParams, body: Body) -> Record {
        let payload = signing_payload(&params.game_id(), &body);
        let sig: Signature = key.sign(&payload);
        Record {
            body,
            signer: key.verifying_key().to_bytes(),
            sig: sig.to_bytes().to_vec(),
        }
    }

    /// Structural validity: is this a real statement by one of the two players?
    ///
    /// Deliberately does NOT check chess legality. An illegal move is a
    /// well-formed statement that the projection ignores — if illegality made
    /// the whole state invalid, either player could destroy the game by
    /// signing garbage.
    pub fn verify(&self, params: &GameParams) -> bool {
        if params.color_of(&self.signer).is_none() {
            return false;
        }
        // Structural, and deliberately before any signature work: a pure
        // per-record predicate, so it distributes over merge and cannot cause
        // the partial-state divergence a chain-length-dependent rule would.
        if self.body.ply().is_some_and(|p| p > MAX_PLY) {
            return false;
        }
        if self.sig.len() != 64 {
            return false;
        }
        let Ok(vk) = VerifyingKey::from_bytes(&self.signer) else {
            return false;
        };
        let Ok(sig) = Signature::from_slice(&self.sig) else {
            return false;
        };
        let payload = signing_payload(&params.game_id(), &self.body);
        vk.verify(&payload, &sig).is_ok()
    }

    pub fn color(&self, params: &GameParams) -> Option<Color> {
        params.color_of(&self.signer)
    }
}
