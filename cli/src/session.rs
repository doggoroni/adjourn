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
//!
//! The rest of this module is the move flow: play a move, resign, offer or
//! accept a draw, or just read the current status. All of them go through
//! [`bound_game`] to turn a `label` into the `GameParams` and contract id the
//! delegate already recorded at bind time (see the `params`/`contract`
//! fields on `GameSummary`), then GET the contract, `project` it, and — for
//! `play_move` — run local pre-checks before ever bothering the delegate.

use adjourn_core::delegate_api::{GameSummary, Refusal, Request, Response, Side};
use adjourn_core::{legal_moves, project, Body, GameParams, GameState, Status};
use anyhow::{bail, Context};
use freenet_stdlib::prelude::{ContractContainer, ContractInstanceId};
use rand::RngCore;
use shakmaty::Color;

use crate::invite::{GameOffer, Invite};
use crate::node::{contract_container, NodeClient};

fn random_entropy() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

fn refused(refusal: Refusal) -> anyhow::Error {
    anyhow::anyhow!("delegate refused: {refusal}")
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

/// Turn a `label` into the game the delegate already recorded for it.
///
/// `GameSummary.params`/`.contract` are what close the loop: `game_id` is a
/// one-way hash, so a caller holding only a label has no way to recover the
/// `GameParams` `project` needs, or the contract id (`hash(code,
/// cbor(params))`), without the delegate handing them back.
async fn bound_game<N: NodeClient>(node: &mut N, label: &str) -> anyhow::Result<GameSummary> {
    let response = node
        .delegate(Request::ListGames)
        .await
        .context("ListGames")?;
    let games = match response {
        Response::Games(games) => games,
        Response::Refused(r) => return Err(refused(r)),
        other => bail!("unexpected response to ListGames: {other:?}"),
    };
    let summary = games
        .into_iter()
        .find(|g| g.label == label)
        .ok_or_else(|| anyhow::anyhow!("no key exists for label {label:?}"))?;
    if summary.game_id.is_none()
        || summary.side.is_none()
        || summary.params.is_none()
        || summary.contract.is_none()
    {
        bail!("label {label:?} has a key but no game bound yet");
    }
    Ok(summary)
}

/// Derive the contract's key and container from `params`, and confirm the id
/// matches what the delegate recorded at bind time. A mismatch means this
/// build's `contract_wasm` differs from the one used when the game was
/// bound, which would otherwise silently GET, PUT and UPDATE the wrong
/// contract.
fn expected_container(
    contract_wasm: Vec<u8>,
    params: &GameParams,
    bound_contract: [u8; 32],
) -> anyhow::Result<ContractContainer> {
    let (container, id) =
        contract_container(contract_wasm, params).context("deriving contract id")?;
    if *id != bound_contract {
        bail!(
            "build mismatch: this build derives contract {} from the bound params, \
             but the delegate recorded {}. Rebuild with the same adjourn-contract \
             version used when this game was bound.",
            ContractInstanceId::new(*id).encode(),
            ContractInstanceId::new(bound_contract).encode(),
        );
    }
    Ok(container)
}

/// GET the contract's raw state and decode it. Empty bytes (a freshly PUT
/// contract nobody has moved in yet) decode as the empty state, not an
/// error, mirroring `adjourn_contract`'s own `decode_state`.
async fn fetch_state<N: NodeClient>(node: &mut N, contract: [u8; 32]) -> anyhow::Result<GameState> {
    let bytes = node
        .get(ContractInstanceId::new(contract), false)
        .await
        .context("GET contract")?
        .unwrap_or_default();
    if bytes.is_empty() {
        return Ok(GameState::empty());
    }
    GameState::decode(&bytes)
        .ok_or_else(|| anyhow::anyhow!("contract state did not decode as a GameState"))
}

/// Everything a command needs to act on a bound game: the delegate's record
/// of it, the contract to address, and the position as currently projected.
///
/// Built by [`open_game`], which every move-flow command goes through so the
/// "pull the summary apart, check the build, GET, project" sequence exists
/// in exactly one place. `open_game` does NOT run any command-specific
/// pre-check (turn order, legality, "already over") — those differ per
/// command (`sign_move_at_ply` skips them all on purpose) and stay in each
/// caller.
struct OpenGame {
    params: GameParams,
    game_id: [u8; 32],
    container: ContractContainer,
    contract: [u8; 32],
    side: Side,
    state: GameState,
    status: Status,
}

/// Turn a `label` into everything a move-flow command needs: resolve the
/// bound game, confirm this build's `contract_wasm` derives the same
/// contract id the delegate recorded at bind time, then GET and project the
/// current state.
async fn open_game<N: NodeClient>(
    node: &mut N,
    label: &str,
    contract_wasm: Vec<u8>,
) -> anyhow::Result<OpenGame> {
    let game = bound_game(node, label).await?;
    let params = game
        .params
        .clone()
        .expect("bound_game checked this is Some");
    let contract = game.contract.expect("bound_game checked this is Some");
    let game_id = game.game_id.expect("bound_game checked this is Some");
    let side = game.side.expect("bound_game checked this is Some");
    let container = expected_container(contract_wasm, &params, contract)?;

    let state = fetch_state(node, contract).await?;
    let status = project(&state, &params);

    Ok(OpenGame {
        params,
        game_id,
        container,
        contract,
        side,
        state,
        status,
    })
}

/// Ask the delegate to sign `body`, submit it as a one-record delta, and
/// return the freshly projected [`Status`].
///
/// This is the only place that talks to the delegate's `Sign` request and to
/// `NodeClient::update`, so every caller — `play_move`, `resign`,
/// `draw_offer`, `draw_accept`, and the test-only `sign_move_at_ply` bypass —
/// goes through one path to the actual guarantee: the delegate refuses a
/// body it should not sign (most importantly, a second different move at a
/// ply it already signed) regardless of what any caller's local checks did
/// or did not verify first.
async fn sign_and_submit<N: NodeClient>(
    node: &mut N,
    game_id: [u8; 32],
    container: ContractContainer,
    params: &GameParams,
    contract: [u8; 32],
    body: Body,
) -> anyhow::Result<Status> {
    let response = node
        .delegate(Request::Sign { game_id, body })
        .await
        .context("Sign")?;
    let record = match response {
        Response::Signed { record } => record,
        Response::Refused(r) => return Err(refused(r)),
        other => bail!("unexpected response to Sign: {other:?}"),
    };

    let mut delta = Vec::new();
    ciborium::into_writer(&vec![record], &mut delta).context("encode delta")?;

    node.update(container.key(), delta)
        .await
        .context("UPDATE contract")?;

    let state = fetch_state(node, contract).await?;
    Ok(project(&state, params))
}

/// Read the current status for `label` without attempting any move.
pub async fn show_label<N: NodeClient>(
    node: &mut N,
    label: &str,
    contract_wasm: Vec<u8>,
) -> anyhow::Result<Status> {
    let g = open_game(node, label, contract_wasm).await?;
    Ok(g.status)
}

/// Play `uci` for `label`.
///
/// The pre-checks below (game not over, our turn, move legal) exist only to
/// give the user a good error before bothering the delegate. They are **not**
/// the guarantee. A client running on a stale view could pass every one of
/// them and still be handing the delegate a second, different move at a ply
/// it already signed -- the fraud proof that forfeits the signer. The
/// delegate's monotonic ply counter (`decide_sign` in
/// `adjourn_core::delegate_policy`) is what actually refuses that; this
/// function only tries to avoid asking.
pub async fn play_move<N: NodeClient>(
    node: &mut N,
    label: &str,
    uci: &str,
    contract_wasm: Vec<u8>,
) -> anyhow::Result<Status> {
    let g = open_game(node, label, contract_wasm).await?;

    if g.status.is_over() {
        bail!("the game is already over");
    }
    if Side::from(g.status.turn) != g.side {
        bail!("it is not your turn");
    }
    if !legal_moves(&g.state, &g.params).iter().any(|m| m == uci) {
        bail!("{uci} is not a legal move in the current position");
    }

    let parent = g
        .status
        .chain
        .last()
        .copied()
        .unwrap_or_else(|| g.params.genesis());
    let body = Body::Move {
        ply: g.status.ply + 1,
        parent,
        uci: uci.to_string(),
    };

    sign_and_submit(node, g.game_id, g.container, &g.params, g.contract, body).await
}

/// Resign `label`. Unconditional and position-independent (see
/// `Body::Resign`), so the only local pre-check is that the game is not
/// already decided.
pub async fn resign<N: NodeClient>(
    node: &mut N,
    label: &str,
    contract_wasm: Vec<u8>,
) -> anyhow::Result<Status> {
    let g = open_game(node, label, contract_wasm).await?;
    if g.status.is_over() {
        bail!("the game is already over");
    }

    sign_and_submit(
        node,
        g.game_id,
        g.container,
        &g.params,
        g.contract,
        Body::Resign,
    )
    .await
}

/// Offer a draw in `label`, anchored to the current head so it implicitly
/// expires once the game moves on (see invariant 9 in `CLAUDE.md`).
pub async fn draw_offer<N: NodeClient>(
    node: &mut N,
    label: &str,
    contract_wasm: Vec<u8>,
) -> anyhow::Result<Status> {
    let g = open_game(node, label, contract_wasm).await?;
    if g.status.is_over() {
        bail!("the game is already over");
    }
    let at = g
        .status
        .chain
        .last()
        .copied()
        .unwrap_or_else(|| g.params.genesis());

    sign_and_submit(
        node,
        g.game_id,
        g.container,
        &g.params,
        g.contract,
        Body::DrawOffer {
            ply: g.status.ply,
            at,
        },
    )
    .await
}

/// Accept the opponent's live draw offer in `label` -- one anchored to the
/// current head, per invariant 9. Refuses rather than guessing if there is
/// none.
pub async fn draw_accept<N: NodeClient>(
    node: &mut N,
    label: &str,
    contract_wasm: Vec<u8>,
) -> anyhow::Result<Status> {
    let g = open_game(node, label, contract_wasm).await?;
    if g.status.is_over() {
        bail!("the game is already over");
    }
    let head = g
        .status
        .chain
        .last()
        .copied()
        .unwrap_or_else(|| g.params.genesis());
    let our_color: Color = g.side.into();

    let offer = g
        .state
        .records
        .iter()
        .find(|(_, rec)| {
            matches!(&rec.body, Body::DrawOffer { at, .. } if *at == head)
                && rec.color(&g.params) == Some(!our_color)
        })
        .map(|(id, _)| *id)
        .ok_or_else(|| anyhow::anyhow!("no live draw offer from your opponent to accept"))?;

    sign_and_submit(
        node,
        g.game_id,
        g.container,
        &g.params,
        g.contract,
        Body::DrawAccept {
            ply: g.status.ply,
            offer,
        },
    )
    .await
}

/// Claim a draw by threefold repetition or the fifty-move rule.
///
/// Both are FIDE *claims* (9.2, 9.3), not automatic results, so nothing happens
/// until a player asks. Checked locally first: a claim with no ground is ignored
/// at projection anyway, so signing one would just add a dead record to state.
pub async fn draw_claim<N: NodeClient>(
    node: &mut N,
    label: &str,
    contract_wasm: Vec<u8>,
) -> anyhow::Result<Status> {
    let g = open_game(node, label, contract_wasm).await?;
    if g.status.is_over() {
        bail!("the game is already over");
    }
    let our_color: Color = g.side.into();
    if g.status.turn != our_color {
        bail!("only the player to move may claim a draw");
    }
    if g.status.repetitions < 3 && g.status.halfmove_clock < 100 {
        bail!(
            "no draw to claim: {} repetitions, {} halfmoves since a capture or pawn move",
            g.status.repetitions,
            g.status.halfmove_clock
        );
    }
    let at = g
        .status
        .chain
        .last()
        .copied()
        .unwrap_or_else(|| g.params.genesis());

    sign_and_submit(
        node,
        g.game_id,
        g.container,
        &g.params,
        g.contract,
        Body::DrawClaim {
            ply: g.status.ply,
            at,
        },
    )
    .await
}

/// Test-only bypass around `play_move`'s local pre-checks.
///
/// It still GETs and projects the current state (so `parent` is correct for
/// `ply`), but it does NOT check whose turn it is, whether the game is over,
/// or whether `uci` is legal -- it signs whatever `ply`/`uci` the caller asks
/// for. That is deliberate: it exists so a test can drive a double-sign
/// attempt straight at the delegate, proving that the delegate's own
/// monotonic ply counter refuses it, rather than the refusal being masked by
/// this client's own guard.
#[doc(hidden)]
pub async fn sign_move_at_ply<N: NodeClient>(
    node: &mut N,
    label: &str,
    ply: u16,
    uci: &str,
    contract_wasm: Vec<u8>,
) -> anyhow::Result<Status> {
    let g = open_game(node, label, contract_wasm).await?;

    // The parent for `ply` is whatever record precedes it in the chain, NOT
    // necessarily the current head: a double-sign attempt at an
    // already-signed ply must reuse that ply's original parent, not the
    // chain's head after it advanced past it.
    let parent = if ply <= 1 {
        g.params.genesis()
    } else {
        *g.status
            .chain
            .get(ply as usize - 2)
            .ok_or_else(|| anyhow::anyhow!("no record at ply {} to parent ply {ply} on", ply - 1))?
    };
    let body = Body::Move {
        ply,
        parent,
        uci: uci.to_string(),
    };

    sign_and_submit(node, g.game_id, g.container, &g.params, g.contract, body)
        .await
        .with_context(|| format!("sign move at ply {ply}"))
}
