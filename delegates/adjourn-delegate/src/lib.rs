//! The Freenet delegate that holds per-game signing keys.
//!
//! All policy lives in `adjourn_core::delegate_policy`, which is pure and tested
//! standalone. This crate is the adapter: secret-store I/O, host entropy, and
//! message dispatch.

pub mod secrets;

use adjourn_core::delegate_api::{EntropyQuality, GameSummary, Refusal, Request, Response};
use adjourn_core::delegate_policy::{
    classify_host_entropy, decide_bind, decide_sign, derive_seed, BindDecision, SignDecision,
};
use adjourn_core::types::{GameParams, Record};
use adjourn_core::{project, Body, GameState};
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
fn probe_host_entropy() -> adjourn_core::delegate_policy::HostEntropy {
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
    origin: Option<[u8; 32]>,
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

    let mut quality_buf = Vec::new();
    if ciborium::into_writer(&quality, &mut quality_buf).is_err() {
        return Response::Refused(Refusal::StoreFailed);
    }
    if !ctx.set_secret(&secrets::key_secret(&label), &seed)
        || !ctx.set_secret(&secrets::quality_secret(&label), &quality_buf)
    {
        return Response::Refused(Refusal::StoreFailed);
    }
    // Only store owner when origin is Some.
    if let Some(origin_id) = origin {
        if !ctx.set_secret(&secrets::owner_secret(&label), &origin_id) {
            return Response::Refused(Refusal::StoreFailed);
        }
    }
    Response::GameKey {
        label,
        public_key,
        entropy: quality,
    }
}

fn handle_bind_game(
    ctx: &mut DelegateCtx,
    origin: Option<[u8; 32]>,
    label: String,
    params: GameParams,
    contract: [u8; 32],
) -> Response {
    let Some(seed) = secrets::load_seed(ctx, &label) else {
        return Response::Refused(Refusal::UnknownLabel);
    };
    let public_key = SigningKey::from_bytes(&seed).verifying_key().to_bytes();

    let existing =
        secrets::load_bound_game_id(ctx, &label).and_then(|id| secrets::load_game(ctx, &id));

    // The game record is keyed by game_id alone, so one delegate holding BOTH
    // sides of a game would have the second bind overwrite the first player's
    // ply counter — silently locking that player out of their own game. Refuse
    // instead. (Re-keying the store per side is the real fix; this closes the
    // data-loss path without an API change.)
    if existing.is_none() {
        if let Some(other) = secrets::load_game(ctx, &params.game_id()) {
            if other.label != label {
                return Response::Refused(Refusal::AlreadyBound {
                    game_id: params.game_id(),
                });
            }
        }
    }

    let quality = secrets::load_quality(ctx, &label).unwrap_or(EntropyQuality::Degraded);

    match decide_bind(
        existing.as_ref(),
        &label,
        public_key,
        &params,
        contract,
        quality,
        origin,
    ) {
        BindDecision::Refuse(refusal) => Response::Refused(refusal),
        BindDecision::Bind { record } => {
            let game_id = record.game_id();
            if !secrets::store_game(ctx, &record) {
                return Response::Refused(Refusal::StoreFailed);
            }
            Response::Bound { game_id }
        }
    }
}

/// Best-effort only. Returns `None` when we cannot tell — no local replica, or
/// it does not decode — and the signature is granted anyway. The monotonic ply
/// counter in `decide_sign` is the actual guarantee; requiring state here would
/// let a cold cache lock a player out of their own game.
fn locally_known_to_be_illegal(
    ctx: &DelegateCtx,
    record: &adjourn_core::delegate_policy::GameRecord,
    body: &Body,
) -> bool {
    let Body::Move { ply, uci, .. } = body else {
        return false;
    };
    // `record.contract`, NOT `record.game_id()`: a contract instance id is
    // hash(code, params) and is a different value from our game id.
    let Some(bytes) = ctx.get_contract_state(&record.contract) else {
        return false;
    };
    let Some(state) = GameState::decode(&bytes) else {
        return false;
    };
    let status = project(&state, &record.params);
    if status.is_over() {
        return true;
    }
    // Only judge when the local replica agrees about which ply is next; if it
    // is behind, we have nothing useful to say.
    if status.ply + 1 != *ply {
        return false;
    }
    !adjourn_core::legal_moves(&state, &record.params)
        .iter()
        .any(|m| m == uci)
}

fn handle_sign(
    ctx: &mut DelegateCtx,
    origin: Option<[u8; 32]>,
    game_id: [u8; 32],
    body: Body,
) -> Response {
    let Some(record) = secrets::load_game(ctx, &game_id) else {
        return Response::Refused(Refusal::UnknownGame);
    };
    let Some(seed) = secrets::load_seed(ctx, &record.label) else {
        return Response::Refused(Refusal::UnknownLabel);
    };

    if locally_known_to_be_illegal(ctx, &record, &body) {
        return Response::Refused(Refusal::IllegalMove);
    }

    match decide_sign(&record, &body, origin) {
        SignDecision::Refuse(refusal) => Response::Refused(refusal),
        SignDecision::Sign { updated } => {
            // Persist BEFORE handing out the signature. If the store write
            // fails we must not release a signature whose ply we did not
            // record, or a retry could produce a different move at that ply.
            if !secrets::store_game(ctx, &updated) {
                return Response::Refused(Refusal::StoreFailed);
            }
            let key = SigningKey::from_bytes(&seed);
            Response::Signed {
                record: Record::sign(&key, &record.params, body),
            }
        }
    }
}

fn handle_list_games(ctx: &DelegateCtx, origin: Option<[u8; 32]>) -> Response {
    let mut games = Vec::new();
    for label in secrets::list_labels(ctx) {
        // Only labels created by this same origin (if any) are visible. Otherwise any
        // web app on the node could enumerate every label and public key the
        // user holds across all their chess identities.
        if secrets::load_owner(ctx, &label) != origin {
            continue;
        }
        let Some(seed) = secrets::load_seed(ctx, &label) else {
            continue;
        };
        let public_key = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
        let quality = secrets::load_quality(ctx, &label);
        let bound =
            secrets::load_bound_game_id(ctx, &label).and_then(|id| secrets::load_game(ctx, &id));
        games.push(match bound {
            Some(record) => GameSummary {
                label,
                public_key,
                game_id: Some(record.game_id()),
                side: Some(record.side),
                last_signed_ply: record.last_signed_ply,
                entropy: Some(record.entropy),
            },
            None => GameSummary {
                label,
                public_key,
                game_id: None,
                side: None,
                last_signed_ply: 0,
                entropy: quality,
            },
        });
    }
    Response::Games(games)
}

fn handle(ctx: &mut DelegateCtx, origin: Option<[u8; 32]>, request: Request) -> Response {
    match request {
        Request::CreateGameKey {
            label,
            caller_entropy,
        } => handle_create_game_key(ctx, origin, label, caller_entropy),
        Request::BindGame {
            label,
            params,
            contract,
        } => handle_bind_game(ctx, origin, label, params, contract),
        Request::Sign { game_id, body } => handle_sign(ctx, origin, game_id, body),
        Request::ListGames => handle_list_games(ctx, origin),
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
