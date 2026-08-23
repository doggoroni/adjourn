//! Secret-store key naming and typed access.
//!
//! Layout:
//! ```text
//! chess/key/<label>     -> 32 raw signing-key bytes
//! chess/bind/<label>    -> 32-byte game_id
//! chess/game/<game_id>  -> CBOR(GameRecord)
//! chess/owner/<label>   -> 32-byte origin (contract instance id) that
//!                          created the key, so CreateGameKey/ListGames can be
//!                          scoped to the web app that created the label
//! chess/quality/<label> -> CBOR(EntropyQuality) recorded at CreateGameKey
//!                          time, so BindGame can carry it into GameRecord
//! ```
//!
//! `chess/bind/` exists because binding is looked up by LABEL while game
//! records are keyed by game id.

use adjourn_core::delegate_api::{EntropyQuality, GameId};
use adjourn_core::delegate_policy::GameRecord;
use freenet_stdlib::prelude::DelegateCtx;

pub const KEY_PREFIX: &[u8] = b"chess/key/";
pub const BIND_PREFIX: &[u8] = b"chess/bind/";
pub const GAME_PREFIX: &[u8] = b"chess/game/";
pub const OWNER_PREFIX: &[u8] = b"chess/owner/";
pub const QUALITY_PREFIX: &[u8] = b"chess/quality/";

pub fn key_secret(label: &str) -> Vec<u8> {
    [KEY_PREFIX, label.as_bytes()].concat()
}

pub fn bind_secret(label: &str) -> Vec<u8> {
    [BIND_PREFIX, label.as_bytes()].concat()
}

pub fn game_secret(game_id: &GameId) -> Vec<u8> {
    [GAME_PREFIX, game_id.as_slice()].concat()
}

pub fn owner_secret(label: &str) -> Vec<u8> {
    [OWNER_PREFIX, label.as_bytes()].concat()
}

pub fn quality_secret(label: &str) -> Vec<u8> {
    [QUALITY_PREFIX, label.as_bytes()].concat()
}

/// The 32 raw signing-key bytes for `label`, if we hold them.
pub fn load_seed(ctx: &DelegateCtx, label: &str) -> Option<[u8; 32]> {
    let bytes = ctx.get_secret(&key_secret(label))?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

pub fn load_bound_game_id(ctx: &DelegateCtx, label: &str) -> Option<GameId> {
    let bytes = ctx.get_secret(&bind_secret(label))?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

pub fn load_game(ctx: &DelegateCtx, game_id: &GameId) -> Option<GameRecord> {
    let bytes = ctx.get_secret(&game_secret(game_id))?;
    ciborium::from_reader(bytes.as_slice()).ok()
}

/// The origin (contract instance id) that created the key for `label`, if any.
pub fn load_owner(ctx: &DelegateCtx, label: &str) -> Option<[u8; 32]> {
    let bytes = ctx.get_secret(&owner_secret(label))?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

/// The entropy quality recorded for `label` at `CreateGameKey` time, if any.
pub fn load_quality(ctx: &DelegateCtx, label: &str) -> Option<EntropyQuality> {
    let bytes = ctx.get_secret(&quality_secret(label))?;
    ciborium::from_reader(bytes.as_slice()).ok()
}

/// Writes the game record and the label -> game_id index together. Returns
/// false if either write fails.
pub fn store_game(ctx: &mut DelegateCtx, record: &GameRecord) -> bool {
    let mut buf = Vec::new();
    if ciborium::into_writer(record, &mut buf).is_err() {
        return false;
    }
    let game_id = record.game_id();
    ctx.set_secret(&game_secret(&game_id), &buf)
        && ctx.set_secret(&bind_secret(&record.label), &game_id)
}

/// Labels we hold a key for, recovered from the `chess/key/` prefix.
pub fn list_labels(ctx: &DelegateCtx) -> Vec<String> {
    ctx.list_secrets(KEY_PREFIX)
        .into_iter()
        .filter_map(|k| {
            let suffix = k.strip_prefix(KEY_PREFIX)?;
            String::from_utf8(suffix.to_vec()).ok()
        })
        .collect()
}
