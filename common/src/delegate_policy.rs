//! The delegate's decision functions.
//!
//! Pure: no I/O, no clock, no randomness. Everything the delegate decides is
//! decided here, so it can be tested on any platform — the delegate crate
//! itself cannot even be compiled on a Windows host.

use crate::delegate_api::{EntropyQuality, Refusal, Side};
use crate::types::{color_at_ply, Body, GameParams, KeyBytes, MAX_PLY};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const DOMAIN_KEYGEN: &[u8] = b"adjourn-v1/keygen";

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
/// so it alone gives "the UI cannot learn the key at generation time". When
/// host entropy is Dead, EVERY input to the seed — the domain tag, the caller
/// draw, the label — is caller-known, so a `Degraded` key is secret only if
/// the caller discards its own contribution afterwards; the delegate has no
/// way to enforce that a caller does this, or to detect it if they don't.
/// With no entropy source at all, this fails closed rather than producing a
/// guessable key.
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

const DOMAIN_BODY: &[u8] = b"adjourn-v1/delegate-body";

/// Layout version of a persisted [`GameRecord`].
///
/// The delegate's secrets OUTLIVE the delegate. `DelegateRequest::RegisterDelegate`
/// carries a `predecessors` list, and the node copies LOCAL-scope secrets forward
/// into the new generation's namespace — that is the designed upgrade path, so a
/// future delegate WILL read records this one wrote.
///
/// The failure to defend against is not a decode error but a decode *success*:
/// add a `#[serde(default)]` field in some later version and serde will happily
/// deserialize an old record with `last_signed_ply` defaulted to 0, silently
/// resetting the double-sign guard on a real in-progress game. So the version is
/// explicit and checked before any other field is trusted.
///
/// Bump this whenever `GameRecord`'s layout changes, and teach the reader how to
/// migrate the old shape rather than widening the check.
pub const GAME_RECORD_FORMAT: u8 = 3;

/// What the delegate knows about one game. Persisted in the secret store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameRecord {
    /// Always [`GAME_RECORD_FORMAT`] when written. Checked before anything
    /// else on read.
    #[serde(rename = "v")]
    pub format: u8,
    pub label: String,
    pub params: GameParams,
    pub side: Side,
    /// Contract instance id of the web app that bound this game, or `None` if
    /// it was bound by a client the runtime attests no origin for — a CLI over
    /// the WebSocket API, for instance.
    ///
    /// Matched EXACTLY on every later call. A web-app game keeps full
    /// protection; a `None` game refuses any caller that presents an origin.
    /// For `None` games the real boundary is the node's own access control:
    /// its WS API binds loopback-only and warns that anything reaching it can
    /// read and modify keys.
    #[serde(with = "serde_bytes")]
    pub origin: Option<[u8; 32]>,
    /// Contract instance id of the GAME, supplied at bind time. Used only to
    /// read local state for the best-effort legality check.
    #[serde(with = "serde_bytes")]
    pub contract: [u8; 32],
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
    /// Quality of the entropy this game's key was generated with. Recorded
    /// because a `Degraded` key's security properties differ from a
    /// `HostBacked` one, and that difference must survive a dropped
    /// `CreateGameKey` response — the UI may never see the original
    /// `Response::GameKey { entropy, .. }` again, but `ListGames` can still
    /// report it from here.
    pub entropy: EntropyQuality,
    /// Highest ply signed so far. 0 means none; plies are 1-indexed.
    pub last_signed_ply: u16,
    /// Body hash of the move signed at `last_signed_ply`, so an identical
    /// retry can be told apart from a different move at the same ply.
    #[serde(with = "serde_bytes")]
    pub last_move_body_hash: [u8; 32],
}

impl GameRecord {
    pub fn game_id(&self) -> [u8; 32] {
        self.params.game_id()
    }
}

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
    entropy: EntropyQuality,
    origin: Option<[u8; 32]>,
) -> BindDecision {
    // Before anything else: if the record's layout is not ours we cannot trust
    // a single field inside it, including the origin.
    if let Some(existing) = existing {
        if existing.format != GAME_RECORD_FORMAT {
            return BindDecision::Refuse(Refusal::StaleRecordFormat {
                found: existing.format,
                expected: GAME_RECORD_FORMAT,
            });
        }
        if existing.origin != origin {
            return BindDecision::Refuse(Refusal::WrongOrigin);
        }
        if existing.game_id() != params.game_id() {
            return BindDecision::Refuse(Refusal::AlreadyBound {
                game_id: existing.game_id(),
            });
        }
        return BindDecision::Bind {
            record: existing.clone(),
        };
    }

    // Catches a UI pairing the wrong key with the wrong game.
    let Some(color) = params.color_of(&public_key) else {
        return BindDecision::Refuse(Refusal::KeyNotInParams);
    };

    BindDecision::Bind {
        record: GameRecord {
            format: GAME_RECORD_FORMAT,
            label: label.to_string(),
            params: params.clone(),
            side: color.into(),
            origin,
            contract,
            previous: None,
            entropy,
            last_signed_ply: 0,
            last_move_body_hash: [0u8; 32],
        },
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignDecision {
    /// Sign the body and persist `updated`. For an identical retry `updated`
    /// equals the record passed in, so persisting it is a no-op — that keeps
    /// one case out of this API.
    Sign {
        updated: GameRecord,
    },
    Refuse(Refusal),
}

/// Decide whether to sign, using only what the delegate itself has recorded.
///
/// Nothing here trusts the caller's view of the game. That is deliberate: the
/// caller may be replaying a stale position, and the ply counter is what makes
/// that harmless.
pub fn decide_sign(record: &GameRecord, body: &Body, origin: Option<[u8; 32]>) -> SignDecision {
    // Before anything else: if the record's layout is not ours we cannot trust
    // a single field inside it — least of all `last_signed_ply`.
    if record.format != GAME_RECORD_FORMAT {
        return SignDecision::Refuse(Refusal::StaleRecordFormat {
            found: record.format,
            expected: GAME_RECORD_FORMAT,
        });
    }
    if record.origin != origin {
        return SignDecision::Refuse(Refusal::WrongOrigin);
    }

    match body {
        Body::Move { ply, .. } => {
            // Before the ply counter is touched: no record past the cap can
            // ever verify, so signing one produces an unusable signature AND
            // permanently advances `last_signed_ply` past every ply the game
            // can still legitimately reach.
            if *ply > MAX_PLY {
                return SignDecision::Refuse(Refusal::PlyOutOfRange {
                    ply: *ply,
                    max: MAX_PLY,
                });
            }
            let needs: Side = color_at_ply(*ply).into();
            if needs != record.side {
                return SignDecision::Refuse(Refusal::WrongSide {
                    ours: record.side,
                    ply_needs: needs,
                });
            }
            if *ply < record.last_signed_ply {
                return SignDecision::Refuse(Refusal::PlyAlreadySigned { ply: *ply });
            }
            if *ply == record.last_signed_ply {
                // Only an identical retry may pass. A DIFFERENT move at a ply
                // already signed is the double-sign fraud proof: signing it
                // would forfeit us.
                if record.last_signed_ply != 0 && body_hash(body) == record.last_move_body_hash {
                    return SignDecision::Sign {
                        updated: record.clone(),
                    };
                }
                return SignDecision::Refuse(Refusal::PlyAlreadySigned { ply: *ply });
            }
            let mut updated = record.clone();
            updated.last_signed_ply = *ply;
            updated.last_move_body_hash = body_hash(body);
            SignDecision::Sign { updated }
        }
        // Idempotent by record id, so no guard is needed.
        Body::Resign
        | Body::DrawOffer { .. }
        | Body::DrawAccept { .. }
        | Body::DrawClaim { .. } => SignDecision::Sign {
            updated: record.clone(),
        },
    }
}
