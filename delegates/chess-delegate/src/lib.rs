//! The Freenet delegate that holds per-game signing keys.
//!
//! All policy lives in `chess_core::delegate_policy`, which is pure and tested
//! standalone. This crate is the adapter: secret-store I/O, host entropy, and
//! message dispatch.

pub mod secrets;

use chess_core::delegate_api::{Refusal, Request, Response};
use chess_core::delegate_policy::{classify_host_entropy, derive_seed};
use ed25519_dalek::SigningKey;
use freenet_stdlib::prelude::*;
use freenet_stdlib::rand::rand_bytes;

pub struct ChessDelegate;

/// The contract instance id of the calling web app, which the runtime attests.
fn origin_id(origin: Option<MessageOrigin>) -> Option<[u8; 32]> {
    match origin {
        Some(MessageOrigin::WebApp(id)) => <[u8; 32]>::try_from(id.as_bytes()).ok(),
        _ => None,
    }
}

/// Two independent draws, so `classify_host_entropy` can spot a dead source.
fn probe_host_entropy() -> chess_core::delegate_policy::HostEntropy {
    let first = <[u8; 32]>::try_from(rand_bytes(32).as_slice()).unwrap_or([0u8; 32]);
    let second = <[u8; 32]>::try_from(rand_bytes(32).as_slice()).unwrap_or([0u8; 32]);
    classify_host_entropy(first, second)
}

fn reply(response: Response) -> Vec<OutboundDelegateMsg> {
    vec![OutboundDelegateMsg::ApplicationMessage(
        ApplicationMessage::new(response.encode()).processed(true),
    )]
}

fn handle_create_game_key(
    ctx: &mut DelegateCtx,
    label: String,
    caller_entropy: Option<[u8; 32]>,
) -> Response {
    if ctx.get_secret(&secrets::key_secret(&label)).is_some() {
        return Response::Refused(Refusal::LabelExists);
    }
    let (seed, quality) = match derive_seed(probe_host_entropy(), caller_entropy, &label) {
        Ok(v) => v,
        Err(refusal) => return Response::Refused(refusal),
    };
    // Never `SigningKey::generate()` — that would pull an RNG crate in.
    let key = SigningKey::from_bytes(&seed);
    let public_key = key.verifying_key().to_bytes();

    if !ctx.set_secret(&secrets::key_secret(&label), &seed) {
        return Response::Refused(Refusal::Malformed("secret store write failed".into()));
    }
    Response::GameKey {
        label,
        public_key,
        entropy: quality,
    }
}

fn handle(ctx: &mut DelegateCtx, origin: Option<[u8; 32]>, request: Request) -> Response {
    let _ = origin;
    match request {
        Request::CreateGameKey {
            label,
            caller_entropy,
        } => handle_create_game_key(ctx, label, caller_entropy),
        _ => Response::Refused(Refusal::Malformed("not yet implemented".into())),
    }
}

#[delegate]
impl DelegateInterface for ChessDelegate {
    fn process(
        ctx: &mut DelegateCtx,
        _parameters: Parameters<'static>,
        origin: Option<MessageOrigin>,
        message: InboundDelegateMsg,
    ) -> Result<Vec<OutboundDelegateMsg>, DelegateError> {
        match message {
            InboundDelegateMsg::ApplicationMessage(app) => {
                let request = match Request::decode(&app.payload) {
                    Ok(r) => r,
                    Err(refusal) => return Ok(reply(Response::Refused(refusal))),
                };
                Ok(reply(handle(ctx, origin_id(origin), request)))
            }
            // `InboundDelegateMsg` is `#[non_exhaustive]`. Reject unknown
            // variants rather than panicking: a panic inside delegate WASM
            // kills the runtime for this delegate and surfaces as an opaque
            // execution error.
            _ => Err(DelegateError::Other(
                "unsupported inbound delegate message".into(),
            )),
        }
    }
}
