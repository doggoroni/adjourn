//! The delegate's decision functions.
//!
//! Pure: no I/O, no clock, no randomness. Everything the delegate decides is
//! decided here, so it can be tested on any platform — the delegate crate
//! itself cannot even be compiled on a Windows host.

use crate::delegate_api::{EntropyQuality, Refusal, Side};
use crate::types::{Body, GameParams, KeyBytes};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const DOMAIN_KEYGEN: &[u8] = b"freenet-chess-v1/keygen";

/// The result of probing the host RNG.
///
/// `freenet_stdlib::rand::rand_bytes` reads into a zero-initialised buffer via
/// a host import that is a no-op stub off-wasm, so it returns all zeros there
/// with no error. Treating that as entropy would mint a known private key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostEntropy {
    Live([u8; 32]),
    Dead,
}

/// Classify two independent draws from the host RNG.
///
/// All-zeros catches the off-wasm stub. Two identical draws catch a dead or
/// missing host source generally — a live CSPRNG repeats 32 bytes with
/// negligible probability.
pub fn classify_host_entropy(first: [u8; 32], second: [u8; 32]) -> HostEntropy {
    if first == [0u8; 32] || first == second {
        HostEntropy::Dead
    } else {
        HostEntropy::Live(first)
    }
}

/// Mix available entropy sources into a signing-key seed.
///
/// Mixing never loses: the result is at least as unpredictable as the
/// strongest input. Host entropy is the only source the UI does not control,
/// so it alone gives "the UI cannot learn the key at generation time"; caller
/// entropy still gives "the UI cannot learn it afterwards". With neither, this
/// fails closed rather than producing a guessable key.
pub fn derive_seed(
    host: HostEntropy,
    caller: Option<[u8; 32]>,
    label: &str,
) -> Result<([u8; 32], EntropyQuality), Refusal> {
    // A caller sending zeros is not contributing entropy, whatever it thinks.
    let caller = caller.filter(|c| c != &[0u8; 32]);

    let (host_bytes, quality) = match host {
        HostEntropy::Live(h) => (h, EntropyQuality::HostBacked),
        HostEntropy::Dead => {
            if caller.is_none() {
                return Err(Refusal::NoEntropy);
            }
            ([0u8; 32], EntropyQuality::Degraded)
        }
    };

    let mut h = Sha256::new();
    h.update(DOMAIN_KEYGEN);
    h.update(host_bytes);
    h.update(caller.unwrap_or([0u8; 32]));
    h.update((label.len() as u32).to_le_bytes());
    h.update(label.as_bytes());
    Ok((h.finalize().into(), quality))
}

const DOMAIN_BODY: &[u8] = b"freenet-chess-v1/delegate-body";

/// What the delegate knows about one game. Persisted in the secret store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameRecord {
    pub label: String,
    pub params: GameParams,
    pub side: Side,
    /// Contract instance id of the WEB APP that bound this game. Only that app
    /// may ask for signatures on it. Note this is the app's own contract, not
    /// the game's.
    pub origin: [u8; 32],
    /// Contract instance id of the GAME, supplied at bind time. Used only to
    /// read local state for the best-effort legality check.
    pub contract: [u8; 32],
    /// Highest ply signed so far. 0 means none; plies are 1-indexed.
    pub last_signed_ply: u16,
    /// Body hash of the move signed at `last_signed_ply`, so an identical
    /// retry can be told apart from a different move at the same ply.
    pub last_move_body_hash: [u8; 32],
}

impl GameRecord {
    pub fn game_id(&self) -> [u8; 32] {
        self.params.game_id()
    }
}

/// Domain-separated hash of a body, used only to recognise an identical retry.
pub fn body_hash(body: &Body) -> [u8; 32] {
    let mut buf = Vec::new();
    ciborium::into_writer(body, &mut buf).expect("cbor encode");
    let mut h = Sha256::new();
    h.update(DOMAIN_BODY);
    h.update(&buf);
    h.finalize().into()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindDecision {
    Bind { record: GameRecord },
    Refuse(Refusal),
}

/// Associate the key held for `label` with a game.
///
/// `existing` is the record currently bound to `label`, if any.
pub fn decide_bind(
    existing: Option<&GameRecord>,
    label: &str,
    public_key: KeyBytes,
    params: &GameParams,
    contract: [u8; 32],
    origin: Option<[u8; 32]>,
) -> BindDecision {
    let Some(origin) = origin else {
        return BindDecision::Refuse(Refusal::MissingOrigin);
    };
    // Catches a UI pairing the wrong key with the wrong game.
    let Some(color) = params.color_of(&public_key) else {
        return BindDecision::Refuse(Refusal::KeyNotInParams);
    };

    if let Some(existing) = existing {
        if existing.game_id() != params.game_id() {
            // Rebinding would orphan the ply counter and reopen the
            // double-sign hole this delegate exists to close.
            return BindDecision::Refuse(Refusal::AlreadyBound {
                game_id: existing.game_id(),
            });
        }
        // Same label, same game: idempotent, for dropped responses.
        return BindDecision::Bind {
            record: existing.clone(),
        };
    }

    BindDecision::Bind {
        record: GameRecord {
            label: label.to_string(),
            params: params.clone(),
            side: color.into(),
            origin,
            contract,
            last_signed_ply: 0,
            last_move_body_hash: [0u8; 32],
        },
    }
}
