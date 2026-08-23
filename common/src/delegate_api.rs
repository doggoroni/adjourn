//! Wire types for the UI <-> delegate protocol.
//!
//! No Freenet dependencies: plain serde types, so the policy layer and its
//! tests build standalone. Deliberately does NOT serialize `shakmaty::Color`
//! (which would need shakmaty's `serde` feature). `Side` is ours, which also
//! keeps the wire format stable across a shakmaty bump.

use crate::types::{Body, GameParams, KeyBytes, Record};
use serde::{Deserialize, Serialize};
use shakmaty::Color;

/// `GameParams::game_id()`.
pub type GameId = [u8; 32];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    White,
    Black,
}

impl From<Color> for Side {
    fn from(c: Color) -> Self {
        match c {
            Color::White => Side::White,
            Color::Black => Side::Black,
        }
    }
}

impl From<Side> for Color {
    fn from(s: Side) -> Self {
        match s {
            Side::White => Color::White,
            Side::Black => Color::Black,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// Create a signing key for a game that does not exist yet. `GameParams`
    /// needs BOTH players' public keys, so your key must exist before the game
    /// does; this returns the half you exchange out of band.
    CreateGameKey {
        label: String,
        #[serde(with = "serde_bytes")]
        caller_entropy: Option<[u8; 32]>,
    },
    /// Record which game the key for `label` belongs to, once both halves are
    /// known.
    ///
    /// `contract` is the game contract's instance id. The delegate cannot
    /// derive it (it is `hash(code, params)`, and the delegate does not have
    /// the contract code), and it is NOT the same as `params.game_id()`. The
    /// UI knows it because it computed that key to PUT the contract. Without
    /// it the delegate has no way to read the game's local state.
    BindGame {
        label: String,
        params: GameParams,
        #[serde(with = "serde_bytes")]
        contract: [u8; 32],
    },
    Sign {
        #[serde(with = "serde_bytes")]
        game_id: GameId,
        body: Body,
    },
    ListGames,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    GameKey {
        label: String,
        #[serde(with = "serde_bytes")]
        public_key: KeyBytes,
        entropy: EntropyQuality,
    },
    Bound {
        #[serde(with = "serde_bytes")]
        game_id: GameId,
    },
    Signed {
        record: Record,
    },
    Games(Vec<GameSummary>),
    Refused(Refusal),
}

/// Whether a key was generated with host-backed randomness. Returned so the UI
/// can warn once, rather than the system quietly pretending all keys are equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntropyQuality {
    HostBacked,
    Degraded,
}

/// Never contains secrets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameSummary {
    pub label: String,
    #[serde(with = "serde_bytes")]
    pub public_key: KeyBytes,
    /// `None` until bound.
    #[serde(with = "serde_bytes")]
    pub game_id: Option<GameId>,
    /// `None` until bound.
    pub side: Option<Side>,
    /// 0 means nothing signed yet; plies are 1-indexed.
    pub last_signed_ply: u16,
    /// `None` for a label that exists but is not bound yet.
    pub entropy: Option<EntropyQuality>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    UnknownLabel,
    LabelExists,
    UnknownGame,
    AlreadyBound {
        #[serde(with = "serde_bytes")]
        game_id: GameId,
    },
    KeyNotInParams,
    WrongSide {
        ours: Side,
        ply_needs: Side,
    },
    PlyAlreadySigned {
        ply: u16,
    },
    MissingOrigin,
    ForeignOrigin,
    NoEntropy,
    Malformed(String),
    StoreFailed,
    IllegalMove,
}

fn encode_cbor<T: Serialize>(value: &T) -> Vec<u8> {
    let mut buf = Vec::new();
    ciborium::into_writer(value, &mut buf).expect("cbor encode");
    buf
}

fn decode_cbor<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, Refusal> {
    ciborium::from_reader(bytes).map_err(|e| Refusal::Malformed(e.to_string()))
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        encode_cbor(self)
    }
    pub fn decode(bytes: &[u8]) -> Result<Request, Refusal> {
        decode_cbor(bytes)
    }
}

impl Response {
    pub fn encode(&self) -> Vec<u8> {
        encode_cbor(self)
    }
    pub fn decode(bytes: &[u8]) -> Result<Response, Refusal> {
        decode_cbor(bytes)
    }
}
