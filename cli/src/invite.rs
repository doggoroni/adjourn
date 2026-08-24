//! The two blobs players copy-paste to agree on a game.
//!
//! Both must end up with byte-identical `GameParams` or they derive different
//! contract ids and sit on separate contracts, each seeing a game the other
//! never joins — with no error anywhere. The nonce therefore has exactly one
//! author, and the offer carries a contract id so a build mismatch is loud:
//! the contract id is `hash(code, params)`, so two players running different
//! `adjourn-contract` builds derive different ids from identical params and
//! land on separate contracts. Carrying the id lets the recipient recompute
//! it and refuse loudly instead of silently playing alone.

use adjourn_core::delegate_api::Side;
use adjourn_core::{GameParams, KeyBytes};
use serde::{Deserialize, Serialize};

pub const INVITE_FORMAT: u8 = 1;
pub const OFFER_FORMAT: u8 = 1;

#[derive(Debug, thiserror::Error)]
pub enum InviteError {
    #[error("not valid base58")]
    Base58,
    #[error("not a valid blob")]
    Malformed,
    #[error("blob is format {found}, this build speaks {expected}")]
    Version { found: u8, expected: u8 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    pub v: u8,
    pub side: Side,
    #[serde(with = "serde_bytes")]
    pub public_key: KeyBytes,
    #[serde(with = "serde_bytes")]
    pub nonce: [u8; 16],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameOffer {
    pub v: u8,
    pub params: GameParams,
    #[serde(with = "serde_bytes")]
    pub contract: [u8; 32],
}

fn encode_blob<T: Serialize>(value: &T) -> String {
    let mut buf = Vec::new();
    ciborium::into_writer(value, &mut buf).expect("cbor encode");
    bs58::encode(buf).into_string()
}

fn decode_blob<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T, InviteError> {
    let bytes = bs58::decode(text.trim())
        .into_vec()
        .map_err(|_| InviteError::Base58)?;
    ciborium::from_reader(bytes.as_slice()).map_err(|_| InviteError::Malformed)
}

impl Invite {
    pub fn new(side: Side, public_key: KeyBytes, nonce: [u8; 16]) -> Self {
        Self {
            v: INVITE_FORMAT,
            side,
            public_key,
            nonce,
        }
    }
    pub fn encode(&self) -> String {
        encode_blob(self)
    }
    pub fn decode(text: &str) -> Result<Self, InviteError> {
        let me: Self = decode_blob(text)?;
        if me.v != INVITE_FORMAT {
            return Err(InviteError::Version {
                found: me.v,
                expected: INVITE_FORMAT,
            });
        }
        Ok(me)
    }
}

impl GameOffer {
    pub fn new(params: GameParams, contract: [u8; 32]) -> Self {
        Self {
            v: OFFER_FORMAT,
            params,
            contract,
        }
    }
    pub fn encode(&self) -> String {
        encode_blob(self)
    }
    pub fn decode(text: &str) -> Result<Self, InviteError> {
        let me: Self = decode_blob(text)?;
        if me.v != OFFER_FORMAT {
            return Err(InviteError::Version {
                found: me.v,
                expected: OFFER_FORMAT,
            });
        }
        Ok(me)
    }
}
