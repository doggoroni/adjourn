use chess_core::delegate_api::{Refusal, Request, Response, Side};
use chess_core::Body;

#[test]
fn requests_round_trip_through_cbor() {
    let req = Request::Sign {
        game_id: [7u8; 32],
        body: Body::Move {
            ply: 3,
            parent: [9u8; 32],
            uci: "e2e4".into(),
        },
    };
    let back = Request::decode(&req.encode()).expect("decode");
    assert_eq!(back, req);
}

#[test]
fn refusals_round_trip_through_cbor() {
    let resp = Response::Refused(Refusal::WrongSide {
        ours: Side::White,
        ply_needs: Side::Black,
    });
    let back = Response::decode(&resp.encode()).expect("decode");
    assert_eq!(back, resp);
}

#[test]
fn malformed_bytes_decode_to_a_refusal_not_a_panic() {
    assert!(matches!(
        Request::decode(&[0xff, 0xff, 0xff]),
        Err(Refusal::Malformed(_))
    ));
}
