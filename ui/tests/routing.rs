use adjourn_client::node::contract_container;
use adjourn_core::GameParams;
use adjourn_ui::node::{route, socket_is_gone, CloseLatch, Frame, Routed};
use freenet_stdlib::client_api::{ClientError, ContractResponse, ErrorKind, HostResponse};
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

fn ok(resp: HostResponse) -> Frame {
    Frame::Result(Ok(resp))
}

#[test]
fn an_update_notification_is_routed_as_a_notification() {
    let resp = HostResponse::ContractResponse(ContractResponse::UpdateNotification {
        key: a_key(),
        update: UpdateData::State(State::from(vec![1, 2, 3])),
    });
    match route(ok(resp)) {
        Routed::Notification(id, UpdateData::State(bytes)) => {
            assert_eq!(id, *a_key().id());
            assert_eq!(bytes.as_ref(), &[1, 2, 3]);
        }
        other => panic!("expected a notification, got {other:?}"),
    }
}

#[test]
fn an_ordinary_response_is_routed_as_a_response() {
    match route(ok(HostResponse::Ok)) {
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
        !matches!(route(ok(resp)), Routed::Response(_)),
        "a notification routed as a response would be consumed by whichever \
         request happened to be waiting, and lost"
    );
}

/// The node reports per-request failures -- a rejected `Update`, a contract
/// execution error -- through the `Err` side of the handler's argument. An
/// earlier version matched `if let Ok(resp)` and dropped them, so the waiting
/// request never woke: a rejected move spun forever with no error anywhere.
#[test]
fn a_node_reported_error_is_routed_as_a_failure() {
    let frame = Frame::Result(Err(ClientError::from(ErrorKind::OperationError {
        cause: "the contract refused the update".into(),
    })));
    match route(frame) {
        Routed::Failed(why) => assert!(
            why.contains("the contract refused the update"),
            "the failure must carry the node's own words, got {why:?}"
        ),
        other => panic!("expected a failure, got {other:?}"),
    }
}

/// A failure is not a response. If it routed as one, the request loop would
/// fall through to its `unexpected response` arm at best, and at worst be
/// swallowed by a `{}` skip arm -- back to hanging.
#[test]
fn a_failure_is_never_mistaken_for_a_response_or_a_notification() {
    let frame = Frame::Result(Err(ClientError::from(ErrorKind::NodeUnavailable)));
    assert!(matches!(route(frame), Routed::Failed(_)));
}

/// `onerror` and `onclose` both land in the error handler, which synthesises
/// this frame. It has to route to its own arm: `next_response` turns it into
/// an error, and `next_update` into `Ok(None)`. Before, both of those arms
/// were unreachable dead code, because freenet-stdlib `forget()`s the
/// onmessage closure and so the inbox never ends.
#[test]
fn a_closed_socket_is_routed_as_closed() {
    match route(Frame::Closed("connection closed".into())) {
        Routed::Closed(why) => assert_eq!(why, "connection closed"),
        other => panic!("expected a close, got {other:?}"),
    }
}

/// `freenet-stdlib` calls the SAME error handler for a dead socket and for a
/// frame it merely failed to decode. Only `onclose` (`source: "close"`) and
/// `onerror` (`source: "exec error"`) are the socket dying.
#[test]
fn only_a_real_close_or_socket_error_counts_as_the_socket_being_gone() {
    assert!(socket_is_gone(Some("close")));
    assert!(socket_is_gone(Some("exec error")));
}

/// The other direction, and the one that bites: an undecodable frame or a
/// reassembly failure on a LIVE socket. Reported as `Closed`, each of these
/// would end `watch` silently -- the exact failure `watch` exists to prevent.
#[test]
fn a_decode_or_reassembly_failure_is_not_the_socket_being_gone() {
    for source in [
        "host response decoding",
        "host response deserialization",
        "stream reassembly deserialization",
        "streaming reassembly",
    ] {
        assert!(
            !socket_is_gone(Some(source)),
            "{source:?} happens on a live socket; treating it as a close ends \
             watch with no error surfaced anywhere"
        );
    }
    // The send-side paths tag themselves `origin`, not `source`, and their
    // failure already reaches the caller as `WebApi::send`'s `Err`.
    assert!(!socket_is_gone(None));
}

/// A synthesised failure routes to `Failed`, not `Closed`: an error for a
/// request that is waiting, a skip for `next_update`.
#[test]
fn a_transport_failure_is_routed_as_a_failure_not_a_close() {
    match route(Frame::Failed("host response deserialization".into())) {
        Routed::Failed(why) => assert!(why.contains("deserialization")),
        other => panic!("expected a failure, got {other:?}"),
    }
}

/// `Frame::Closed` is a single queue item: whichever waiter reads it consumes
/// it. Without a latch, a request in flight bails correctly and the NEXT
/// `next_update` parks on an inbox that can never yield again -- no timeout by
/// design, and the stdlib leaks the sender so it never ends. Forever, on a
/// dead socket, showing a stale board.
#[test]
fn a_close_latches_so_a_later_waiter_still_sees_it() {
    let mut latch = CloseLatch::default();
    assert_eq!(latch.why(), None, "a fresh latch must not claim a close");
    latch.observe(&route(Frame::Closed("connection closed".into())));
    assert_eq!(latch.why(), Some("connection closed"));
    // Still latched after the frame has been consumed and forgotten.
    assert_eq!(latch.why(), Some("connection closed"));
}

/// The first reason wins -- `onerror` then `onclose` is one death, and the
/// first message is the one that explains it.
#[test]
fn the_first_close_reason_is_the_one_kept() {
    let mut latch = CloseLatch::default();
    latch.observe(&route(Frame::Closed("exec error: refused".into())));
    latch.observe(&route(Frame::Closed("connection closed".into())));
    assert_eq!(latch.why(), Some("exec error: refused"));
}

/// The interaction between the two findings: a recoverable error must NOT
/// latch. If it did, one undecodable frame would permanently convince every
/// later call that the connection is dead.
#[test]
fn nothing_but_a_close_latches() {
    let mut latch = CloseLatch::default();
    latch.observe(&route(Frame::Failed(
        "host response deserialization".into(),
    )));
    latch.observe(&route(Frame::Result(Err(ClientError::from(
        ErrorKind::NodeUnavailable,
    )))));
    latch.observe(&route(ok(HostResponse::Ok)));
    assert_eq!(
        latch.why(),
        None,
        "a survivable error latched the connection shut"
    );
}
