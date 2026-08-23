//! Adapter-level tests. CI-only: this crate cannot be compiled on a Windows host,
//! because `freenet-stdlib` depends unconditionally on `tracing-subscriber`, which
//! pulls `windows-sys`.
//!
//! Every policy rule is tested in `adjourn-core`, where it runs on any platform. What
//! is checked here is narrower: for one hand-picked label and game id, the
//! `chess/key/`, `chess/bind/` and `chess/game/` secret-store keys they produce
//! are pairwise distinct, and a crafted label cannot forge another namespace's
//! prefix. It is a sanity check on the naming scheme, not a proof that no
//! collision exists for any input.

#![cfg(not(target_arch = "wasm32"))]

use adjourn_delegate::secrets::{bind_secret, game_secret, key_secret, GAME_PREFIX, KEY_PREFIX};

#[test]
fn the_three_namespaces_never_collide_for_one_label() {
    let label = "g1";
    let id = [7u8; 32];
    let k = key_secret(label);
    let b = bind_secret(label);
    let g = game_secret(&id);
    assert_ne!(k, b);
    assert_ne!(k, g);
    assert_ne!(b, g);
}

#[test]
fn a_crafted_label_cannot_forge_another_namespace() {
    // Labels come from the caller, so treat them as hostile.
    let crafted = key_secret("../bind/g1");
    assert!(crafted.starts_with(KEY_PREFIX));
    assert_ne!(crafted, bind_secret("g1"));
}

#[test]
fn distinct_labels_give_distinct_secret_keys() {
    assert_ne!(key_secret("a"), key_secret("b"));
    assert_ne!(bind_secret("a"), bind_secret("b"));
}

#[test]
fn game_secret_embeds_the_raw_game_id() {
    let id = [9u8; 32];
    let s = game_secret(&id);
    assert!(s.starts_with(GAME_PREFIX));
    assert_eq!(&s[GAME_PREFIX.len()..], &id[..]);
}
