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
    Move {
        ply: u16,
        #[serde(with = "serde_bytes")]
        parent: RecordId,
        uci: String,
    },
    /// Unconditional. Position-independent, so it needs no anchor.
    Resign,
    /// A draw offer anchored to a specific head, so it expires implicitly
    /// once the game moves on.
    DrawOffer {
        #[serde(with = "serde_bytes")]
        at: RecordId,
    },
    /// Accepts a specific offer by record id.
    DrawAccept {
        #[serde(with = "serde_bytes")]
        offer: RecordId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub body: Body,
    #[serde(with = "serde_bytes")]
    pub signer: KeyBytes,
    /// 64 raw ed25519 bytes.
    #[serde(with = "serde_bytes")]
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
