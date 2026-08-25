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
    /// The game's parameters, once bound. `None` for a label that has a key
    /// but no game yet.
    ///
    /// Needed because `game_id` is a one-way hash: a caller holding only a
    /// label otherwise has no route back to the params that `project` needs,
    /// nor to the contract id, which is `hash(code, cbor(params))`.
    pub params: Option<GameParams>,
    /// The game contract's instance id, once bound.
    #[serde(with = "serde_bytes")]
    pub contract: Option<[u8; 32]>,
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
    /// The requested ply is past `MAX_PLY`. A record carrying it would be
    /// refused by `Record::verify`, and signing one would permanently advance
    /// `last_signed_ply` past every reachable ply.
    PlyOutOfRange {
        ply: u16,
        max: u16,
    },
    /// The caller is not who bound this game. With `Option` equality there is
    /// exactly one way to fail — you are not the binder — so one variant.
    WrongOrigin,
    NoEntropy,
    /// The persisted record was written by a different delegate generation.
    /// Refused rather than interpreted: see `GAME_RECORD_FORMAT`.
    StaleRecordFormat {
        found: u8,
        expected: u8,
    },
    Malformed(String),
    StoreFailed,
    IllegalMove,
}

/// Human sentences, not debug output — a refusal is an expected outcome the
/// UI shows a user, not a crash. `PlyAlreadySigned` in particular is the one
/// a user hits through legitimate retry, so it reads as an explanation with
/// an action, not a struct dump.
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::UnknownLabel => write!(f, "no key exists for that label"),
            Refusal::LabelExists => write!(f, "a key already exists for that label"),
            Refusal::UnknownGame => write!(f, "no game is bound to that label"),
            Refusal::AlreadyBound { .. } => {
                write!(f, "that label is already bound to a different game")
            }
            Refusal::KeyNotInParams => write!(
                f,
                "this key is not one of the two players named in the game's parameters"
            ),
            Refusal::WrongSide { ours, ply_needs } => {
                write!(f, "it is {ply_needs:?}'s turn, but this key plays {ours:?}")
            }
            Refusal::PlyAlreadySigned { ply } => write!(
                f,
                "you have already signed a different move at ply {ply}; \
                 re-send the identical move, or wait for your opponent"
            ),
            Refusal::PlyOutOfRange { ply, max } => write!(
                f,
                "ply {ply} is past the maximum of {max}; no valid record can carry it"
            ),
            Refusal::WrongOrigin => {
                write!(f, "this game was bound by a different caller")
            }
            Refusal::NoEntropy => {
                write!(f, "no entropy source is available to generate a key")
            }
            Refusal::StaleRecordFormat { found, expected } => write!(
                f,
                "the stored record is format {found}, but this delegate speaks format {expected}"
            ),
            Refusal::Malformed(msg) => write!(f, "malformed request: {msg}"),
            Refusal::StoreFailed => write!(f, "the secret store rejected the write"),
            Refusal::IllegalMove => write!(f, "that move is illegal in the current position"),
        }
    }
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
