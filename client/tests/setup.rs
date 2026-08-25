mod common;

use adjourn_client::fake::{shared_world, FakeNode};
use adjourn_client::session::{game_bind, invite_accept, invite_new};
use adjourn_core::delegate_api::Side;
use freenet_stdlib::prelude::ContractInstanceId;

#[tokio::test]
async fn both_players_derive_the_same_contract() {
    let Some(wasm) = common::contract_wasm() else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let world = shared_world();
    let mut alice = FakeNode::new(world.clone());
    let mut bob = FakeNode::new(world);

    let invite = invite_new(&mut alice, "alice", Side::White).await.unwrap();
    let offer = invite_accept(&mut bob, "bob", &invite, wasm.clone())
        .await
        .unwrap();
    let alice_id = game_bind(&mut alice, "alice", &offer, wasm).await.unwrap();

    assert_eq!(
        alice_id,
        ContractInstanceId::new(offer.contract),
        "the two sides derived different contracts; they would never meet"
    );
}

/// Two players on different adjourn-contract builds derive different ids from
/// identical params, and would sit on separate contracts each seeing a game
/// the other never joins -- with no error anywhere. Make it loud.
#[tokio::test]
async fn a_build_mismatch_is_refused_loudly() {
    let Some(wasm) = common::contract_wasm() else {
        return eprintln!("skipping: run ./scripts/build-contract.sh first");
    };
    let world = shared_world();
    let mut alice = FakeNode::new(world.clone());
    let mut bob = FakeNode::new(world);

    let invite = invite_new(&mut alice, "alice", Side::White).await.unwrap();
    let mut offer = invite_accept(&mut bob, "bob", &invite, wasm.clone())
        .await
        .unwrap();
    offer.contract[0] ^= 0xff;

    let err = game_bind(&mut alice, "alice", &offer, wasm)
        .await
        .expect_err("a corrupted contract id must be refused");
    assert!(
        format!("{err:#}").to_lowercase().contains("build"),
        "the error must name a build mismatch, got: {err:#}"
    );
}
