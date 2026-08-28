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

use std::collections::BTreeMap;

use adjourn_core::delegate_api::{EntropyQuality, GameId};
use adjourn_core::delegate_policy::{migrate_record, GameRecord};
use freenet_stdlib::prelude::DelegateCtx;

/// The delegate's persistence, abstracted so the handlers can run off-wasm.
///
/// `DelegateCtx`'s secret methods are FFI stubs outside WASM — they return
/// `None` and `false` unconditionally — so without this the dispatch code
/// could never be tested on a host, and an in-memory fake would have to
/// reimplement it and drift from it invisibly.
pub trait SecretStore {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>>;
    fn set(&mut self, key: &[u8], value: &[u8]) -> bool;
    fn list(&self, prefix: &[u8]) -> Vec<Vec<u8>>;

    /// Best-effort read of a contract's local state. `None` is a legitimate
    /// answer — the node may simply not hold this contract.
    fn contract_state(&self, id: &[u8; 32]) -> Option<Vec<u8>>;
}

impl SecretStore for DelegateCtx {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.get_secret(key)
    }
    fn set(&mut self, key: &[u8], value: &[u8]) -> bool {
        self.set_secret(key, value)
    }
    fn list(&self, prefix: &[u8]) -> Vec<Vec<u8>> {
        self.list_secrets(prefix)
    }
    fn contract_state(&self, id: &[u8; 32]) -> Option<Vec<u8>> {
        self.get_contract_state(id)
    }
}

/// Not `#[cfg(test)]`: the CLI's `FakeNode` uses it to run the real delegate.
#[derive(Debug, Default, Clone)]
pub struct MemoryStore(BTreeMap<Vec<u8>, Vec<u8>>);

impl SecretStore for MemoryStore {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.0.get(key).cloned()
    }
    fn set(&mut self, key: &[u8], value: &[u8]) -> bool {
        self.0.insert(key.to_vec(), value.to_vec());
        true
    }
    fn list(&self, prefix: &[u8]) -> Vec<Vec<u8>> {
        self.0
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect()
    }
    fn contract_state(&self, _id: &[u8; 32]) -> Option<Vec<u8>> {
        // `MemoryStore` holds delegate secrets only, never contract state.
        // The CLI's `FakeNode` needs a real answer here and supplies its own
        // store type that wraps this one with a `World` handle -- see
        // `cli/src/fake.rs`.
        None
    }
}

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
pub fn load_seed<S: SecretStore>(store: &S, label: &str) -> Option<[u8; 32]> {
    let bytes = store.get(&key_secret(label))?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

pub fn load_bound_game_id<S: SecretStore>(store: &S, label: &str) -> Option<GameId> {
    let bytes = store.get(&bind_secret(label))?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

pub fn load_game<S: SecretStore>(store: &S, game_id: &GameId) -> Option<GameRecord> {
    let bytes = store.get(&game_secret(game_id))?;
    let rec: GameRecord = ciborium::from_reader(bytes.as_slice()).ok()?;
    // ONE migration point, so no caller can forget. The per-decision format
    // checks stay as defence in depth.
    migrate_record(rec)
}

/// The origin (contract instance id) that created the key for `label`, if any.
pub fn load_owner<S: SecretStore>(store: &S, label: &str) -> Option<[u8; 32]> {
    let bytes = store.get(&owner_secret(label))?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

/// The entropy quality recorded for `label` at `CreateGameKey` time, if any.
pub fn load_quality<S: SecretStore>(store: &S, label: &str) -> Option<EntropyQuality> {
    let bytes = store.get(&quality_secret(label))?;
    ciborium::from_reader(bytes.as_slice()).ok()
}

/// Writes the game record and the label -> game_id index together. Returns
/// false if either write fails.
pub fn store_game<S: SecretStore>(store: &mut S, record: &GameRecord) -> bool {
    let mut buf = Vec::new();
    if ciborium::into_writer(record, &mut buf).is_err() {
        return false;
    }
    let game_id = record.game_id();
    store.set(&game_secret(&game_id), &buf) && store.set(&bind_secret(&record.label), &game_id)
}

/// Labels we hold a key for, recovered from the `chess/key/` prefix.
pub fn list_labels<S: SecretStore>(store: &S) -> Vec<String> {
    store
        .list(KEY_PREFIX)
        .into_iter()
        .filter_map(|k| {
            let suffix = k.strip_prefix(KEY_PREFIX)?;
            String::from_utf8(suffix.to_vec()).ok()
        })
        .collect()
}
