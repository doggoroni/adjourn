//! The three setup flows: invite, accept, bind.
//!
//! Both players must land on byte-identical [`GameParams`], or they derive
//! different contract ids, sit on separate contracts, and each sees a game
//! the other never joins — with no error anywhere. The invite carries a
//! nonce authored by exactly one player (the inviter) so the two sides can
//! never disagree on it. The offer carries the accepter's derived contract
//! id so a build mismatch between the two players — different
//! `adjourn-contract` bytes deriving different ids from identical params —
//! is loud rather than silent.

use adjourn_core::delegate_api::{Refusal, Request, Response, Side};
use adjourn_core::GameParams;
use anyhow::{bail, Context};
use freenet_stdlib::prelude::ContractInstanceId;
use rand::RngCore;

use crate::invite::{GameOffer, Invite};
use crate::node::{contract_container, NodeClient};

fn random_entropy() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

fn refused(refusal: Refusal) -> anyhow::Error {
    anyhow::anyhow!("delegate refused: {refusal:?}")
}

/// Ask the delegate for a fresh signing key under `label`, then wrap the
/// public half in an [`Invite`] carrying a nonce this side authors. The
/// nonce has exactly one author so the two sides can never derive different
/// `GameParams` from it.
pub async fn invite_new<N: NodeClient>(
    node: &mut N,
    label: &str,
    side: Side,
) -> anyhow::Result<Invite> {
    let response = node
        .delegate(Request::CreateGameKey {
            label: label.to_string(),
            caller_entropy: Some(random_entropy()),
        })
        .await
        .context("CreateGameKey")?;
    let public_key = match response {
        Response::GameKey { public_key, .. } => public_key,
        Response::Refused(r) => return Err(refused(r)),
        other => bail!("unexpected response to CreateGameKey: {other:?}"),
    };

    let mut nonce = [0u8; 16];
    rand::rng().fill_bytes(&mut nonce);

    Ok(Invite::new(side, public_key, nonce))
}

/// Accept an [`Invite`]: create our own signing key, assemble the
/// `GameParams` the invite's nonce and side pin down, derive the contract id
/// from `contract_wasm`, PUT the contract with empty state, bind the
/// delegate to it, and return the [`GameOffer`] to send back to the
/// inviter.
pub async fn invite_accept<N: NodeClient>(
    node: &mut N,
    label: &str,
    invite: &Invite,
    contract_wasm: Vec<u8>,
) -> anyhow::Result<GameOffer> {
    let response = node
        .delegate(Request::CreateGameKey {
            label: label.to_string(),
            caller_entropy: Some(random_entropy()),
        })
        .await
        .context("CreateGameKey")?;
    let our_public_key = match response {
        Response::GameKey { public_key, .. } => public_key,
        Response::Refused(r) => return Err(refused(r)),
        other => bail!("unexpected response to CreateGameKey: {other:?}"),
    };

    // The invite's `side` names the inviter's colour; we take the other.
    let (white, black) = match invite.side {
        Side::White => (invite.public_key, our_public_key),
        Side::Black => (our_public_key, invite.public_key),
    };
    let params = GameParams {
        white,
        black,
        nonce: invite.nonce,
    };

    let (container, id) =
        contract_container(contract_wasm, &params).context("deriving contract id")?;

    node.put(container, Vec::new())
        .await
        .context("PUT contract")?;

    let bind_response = node
        .delegate(Request::BindGame {
            label: label.to_string(),
            params: params.clone(),
            contract: *id,
        })
        .await
        .context("BindGame")?;
    match bind_response {
        Response::Bound { .. } => {}
        Response::Refused(r) => return Err(refused(r)),
        other => bail!("unexpected response to BindGame: {other:?}"),
    }

    Ok(GameOffer::new(params, *id))
}

/// Accept a [`GameOffer`] on the inviting side: recompute the contract id
/// from our own WASM and refuse loudly if it differs from what the offer
/// carries — a mismatch means the two players are running different
/// `adjourn-contract` builds and would otherwise sit on separate contracts,
/// each seeing a game the other never joins, with no error anywhere. Then
/// fetch the contract (PUTting it with empty state if this side hasn't seen
/// it yet) and bind the delegate to it.
pub async fn game_bind<N: NodeClient>(
    node: &mut N,
    label: &str,
    offer: &GameOffer,
    contract_wasm: Vec<u8>,
) -> anyhow::Result<ContractInstanceId> {
    let (container, id) =
        contract_container(contract_wasm, &offer.params).context("deriving contract id")?;

    if *id != offer.contract {
        bail!(
            "build mismatch: this build derives contract {} from the offered params, \
             but the offer names {}. The two players are running different \
             adjourn-contract builds and would silently sit on separate contracts.",
            ContractInstanceId::new(*id).encode(),
            ContractInstanceId::new(offer.contract).encode(),
        );
    }

    if node.get(id, false).await.context("GET contract")?.is_none() {
        node.put(container, Vec::new())
            .await
            .context("PUT contract")?;
    }

    let bind_response = node
        .delegate(Request::BindGame {
            label: label.to_string(),
            params: offer.params.clone(),
            contract: *id,
        })
        .await
        .context("BindGame")?;
    match bind_response {
        Response::Bound { .. } => {}
        Response::Refused(r) => return Err(refused(r)),
        other => bail!("unexpected response to BindGame: {other:?}"),
    }

    Ok(id)
}
