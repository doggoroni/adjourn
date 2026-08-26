use adjourn_client::node::contract_container;
use adjourn_core::GameParams;
use adjourn_ui::node::{route, Routed};
use freenet_stdlib::client_api::{ContractResponse, HostResponse};
use freenet_stdlib::prelude::*;

/// A real `ContractKey`, taken from a real container.
///
/// `freenet-stdlib` has no `ContractKey::from(ContractInstanceId)` -- only the
/// reverse conversion -- and nothing in this repo builds one any other way.
/// The UI already embeds the contract WASM and `build.rs` guarantees it is
/// present, so this costs nothing and exercises a genuine key.
fn a_key() -> ContractKey {
    let params = GameParams {
        white: [1u8; 32],
        black: [2u8; 32],
        nonce: [7u8; 16],
    };
    let (container, _id) = contract_container(adjourn_ui::CONTRACT_WASM.to_vec(), &params)
        .expect("the embedded contract WASM builds a container");
    container.key()
}

#[test]
fn an_update_notification_is_routed_as_a_notification() {
    let resp = HostResponse::ContractResponse(ContractResponse::UpdateNotification {
        key: a_key(),
        update: UpdateData::State(State::from(vec![1, 2, 3])),
    });
    match route(resp) {
        Routed::Notification(id, UpdateData::State(bytes)) => {
            assert_eq!(id, *a_key().id());
            assert_eq!(bytes.as_ref(), &[1, 2, 3]);
        }
        other => panic!("expected a notification, got {other:?}"),
    }
}

#[test]
fn an_ordinary_response_is_routed_as_a_response() {
    match route(HostResponse::Ok) {
        Routed::Response(HostResponse::Ok) => {}
        other => panic!("expected a response, got {other:?}"),
    }
}

/// A notification arriving while a request is in flight must not be mistaken
/// for that request's answer -- that is what makes `watch` miss moves.
#[test]
fn a_notification_is_never_mistaken_for_a_response() {
    let resp = HostResponse::ContractResponse(ContractResponse::UpdateNotification {
        key: a_key(),
        update: UpdateData::Delta(StateDelta::from(vec![4, 5])),
    });
    assert!(
        !matches!(route(resp), Routed::Response(_)),
        "a notification routed as a response would be consumed by whichever \
         request happened to be waiting, and lost"
    );
}
