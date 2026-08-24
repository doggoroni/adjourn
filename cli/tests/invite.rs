use adjourn_cli::invite::{GameOffer, Invite, InviteError, OFFER_FORMAT};
use adjourn_core::delegate_api::Side;
use adjourn_core::GameParams;

fn params() -> GameParams {
    GameParams {
        white: [1u8; 32],
        black: [2u8; 32],
        nonce: [7u8; 16],
    }
}

#[test]
fn an_invite_round_trips_through_base58() {
    let inv = Invite::new(Side::White, [3u8; 32], [9u8; 16]);
    let back = Invite::decode(&inv.encode()).expect("decode");
    assert_eq!(back, inv);
}

#[test]
fn an_offer_round_trips_through_base58() {
    let offer = GameOffer::new(params(), [4u8; 32]);
    let back = GameOffer::decode(&offer.encode()).expect("decode");
    assert_eq!(back, offer);
}

#[test]
fn a_blob_from_a_future_version_is_refused() {
    let mut offer = GameOffer::new(params(), [4u8; 32]);
    offer.v = OFFER_FORMAT + 1;
    let encoded = offer.encode();
    assert!(matches!(
        GameOffer::decode(&encoded),
        Err(InviteError::Version { .. })
    ));
}

#[test]
fn garbage_is_refused_rather_than_panicking() {
    assert!(Invite::decode("not base58 !!!").is_err());
    assert!(Invite::decode("").is_err());
}
