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

use adjourn_core::delegate_api::{EntropyQuality, Refusal, Request, Response};
use adjourn_core::{Body, GameParams};
use adjourn_delegate::handle;
use adjourn_delegate::secrets::MemoryStore;
use ed25519_dalek::SigningKey;

const CONTRACT: [u8; 32] = [5u8; 32];

fn no_state(_: &[u8; 32]) -> Option<Vec<u8>> {
    None
}

/// The delegate's dispatch has never run outside WASM before. This is the
/// first test of the handlers themselves rather than the policy beneath them.
#[test]
fn a_key_can_be_created_and_listed() {
    let mut store = MemoryStore::default();

    let resp = handle(
        &mut store,
        no_state,
        None,
        Request::CreateGameKey {
            label: "alice".into(),
            caller_entropy: Some([9u8; 32]),
        },
    );
    let Response::GameKey { label, entropy, .. } = resp else {
        panic!("expected a key, got {resp:?}");
    };
    assert_eq!(label, "alice");
    // rand_bytes is a no-op stub off-wasm, so host entropy is dead and the
    // caller's contribution is all there is. Degraded is the honest answer.
    assert_eq!(entropy, EntropyQuality::Degraded);

    let Response::Games(games) = handle(&mut store, no_state, None, Request::ListGames) else {
        panic!("expected a list");
    };
    assert_eq!(games.len(), 1);
    assert_eq!(games[0].label, "alice");
    assert_eq!(games[0].game_id, None, "not bound yet");
}

/// Fail closed: with no host entropy AND no caller entropy there is nothing
/// unpredictable to build a key from.
#[test]
fn creating_a_key_with_no_entropy_at_all_is_refused() {
    let mut store = MemoryStore::default();
    let resp = handle(
        &mut store,
        no_state,
        None,
        Request::CreateGameKey {
            label: "alice".into(),
            caller_entropy: None,
        },
    );
    assert!(matches!(resp, Response::Refused(Refusal::NoEntropy)));
}

#[test]
fn the_same_label_cannot_be_created_twice() {
    let mut store = MemoryStore::default();
    let req = || Request::CreateGameKey {
        label: "alice".into(),
        caller_entropy: Some([9u8; 32]),
    };
    assert!(matches!(
        handle(&mut store, no_state, None, req()),
        Response::GameKey { .. }
    ));
    assert!(matches!(
        handle(&mut store, no_state, None, req()),
        Response::Refused(Refusal::LabelExists)
    ));
}

/// The whole point of the delegate, exercised through the real dispatch path
/// for the first time: a second DIFFERENT move at a signed ply is refused.
#[test]
fn the_dispatch_path_refuses_a_double_sign() {
    let mut store = MemoryStore::default();
    let w = SigningKey::from_bytes(&[1u8; 32]);
    let b = SigningKey::from_bytes(&[2u8; 32]);

    let Response::GameKey { public_key, .. } = handle(
        &mut store,
        no_state,
        None,
        Request::CreateGameKey {
            label: "white".into(),
            caller_entropy: Some([9u8; 32]),
        },
    ) else {
        panic!("expected a key");
    };

    let params = GameParams {
        white: public_key,
        black: b.verifying_key().to_bytes(),
        nonce: [7u8; 16],
    };
    let _ = w;

    let Response::Bound { game_id } = handle(
        &mut store,
        no_state,
        None,
        Request::BindGame {
            label: "white".into(),
            params: params.clone(),
            contract: CONTRACT,
        },
    ) else {
        panic!("expected a bind");
    };

    let mv = |uci: &str| Request::Sign {
        game_id,
        body: Body::Move {
            ply: 1,
            parent: params.genesis(),
            uci: uci.into(),
        },
    };

    assert!(matches!(
        handle(&mut store, no_state, None, mv("e2e4")),
        Response::Signed { .. }
    ));
    // Identical retry: allowed, because a dropped response must not wedge the game.
    assert!(matches!(
        handle(&mut store, no_state, None, mv("e2e4")),
        Response::Signed { .. }
    ));
    // A DIFFERENT move at the same ply: the fraud proof. Refused.
    assert!(matches!(
        handle(&mut store, no_state, None, mv("d2d4")),
        Response::Refused(Refusal::PlyAlreadySigned { ply: 1 })
    ));
}
