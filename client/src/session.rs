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
use adjourn_core::state::Delta;
use adjourn_core::{legal_moves, project, Body, GameParams, GameState, Status};
use anyhow::{anyhow, bail, Context};
use freenet_stdlib::prelude::{ContractContainer, ContractInstanceId, UpdateData};
use shakmaty::Color;

use crate::invite::{GameOffer, Invite};
use crate::node::{contract_container, NodeClient};

fn refused(refusal: Refusal) -> anyhow::Error {
    anyhow::anyhow!("delegate refused: {refusal}")
}

/// Ask the delegate for a fresh signing key under `label`, then wrap the
/// public half in an [`Invite`] carrying `nonce`.
///
/// Both random inputs are **parameters, not generated here**. This crate has
/// to compile for `wasm32-unknown-unknown` (it exists to be reachable from a
/// browser), and every `rand`/`getrandom` path hard-errors on that target --
/// the same dependency `CLAUDE.md` bans anywhere near the contract and
/// delegate graphs, which a workspace-wide feature unification would drag in
/// behind this crate. So the caller supplies the bytes: the CLI from `rand`,
/// the browser from `crypto.getRandomValues`.
///
/// `entropy` is the caller's contribution to the delegate's key derivation
/// (`Request::CreateGameKey.caller_entropy`); the delegate mixes it with host
/// randomness and never trusts it alone. `nonce` goes into `GameParams`.
///
/// Hoisting where the bytes come from does **not** change who authors them:
/// the nonce still has exactly one author, the inviter. That is what stops
/// the two sides deriving different `GameParams`, landing on different
/// contract ids, and each seeing a game the other never joins with no error
/// anywhere.
pub async fn invite_new<N: NodeClient>(
    node: &mut N,
    label: &str,
    side: Side,
    entropy: [u8; 32],
    nonce: [u8; 16],
) -> anyhow::Result<Invite> {
    let response = node
        .delegate(Request::CreateGameKey {
            label: label.to_string(),
            caller_entropy: Some(entropy),
        })
        .await
        .context("CreateGameKey")?;
    let public_key = match response {
        Response::GameKey { public_key, .. } => public_key,
        Response::Refused(r) => return Err(refused(r)),
        other => bail!("unexpected response to CreateGameKey: {other:?}"),
    };

    Ok(Invite::new(side, public_key, nonce))
}

/// Accept an [`Invite`]: create our own signing key, assemble the
/// `GameParams` the invite's nonce and side pin down, derive the contract id
/// from `contract_wasm`, PUT the contract with empty state, bind the
/// delegate to it, and return the [`GameOffer`] to send back to the
/// inviter.
///
/// `entropy` is the caller's contribution to the delegate's key derivation,
/// supplied rather than generated for the reason spelled out on
/// [`invite_new`]. This side authors **no** nonce: it takes the inviter's,
/// which is the whole point of the invite carrying one.
pub async fn invite_accept<N: NodeClient>(
    node: &mut N,
    label: &str,
    invite: &Invite,
    contract_wasm: Vec<u8>,
    entropy: [u8; 32],
) -> anyhow::Result<GameOffer> {
    let response = node
        .delegate(Request::CreateGameKey {
            label: label.to_string(),
            caller_entropy: Some(entropy),
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

/// What a migration did, so a caller can report it precisely rather than
/// saying "ok".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrateOutcome {
    /// This build already derives the recorded id. Nothing to do.
    AlreadyCurrent { contract: [u8; 32] },
    Migrated {
        from: [u8; 32],
        to: [u8; 32],
        records: usize,
    },
}

/// Move an in-progress game onto the contract id THIS build derives.
///
/// Ordered so a failure never leaves a worse state than it found: the PUT
/// happens before the delegate is told anything, so a failed PUT leaves the
/// game exactly where it was, still bound to the old id. If the PUT succeeds
/// and the Rebind does not, the new address holds the state and the delegate
/// still points at the old one -- re-running this completes it, because a PUT
/// of the same records merges by union and a Rebind to the already-current id
/// is a no-op.
///
/// Deliberately goes through [`bound_game`], not [`open_game`]: `open_game`
/// (via `expected_container`) refuses precisely when the recorded contract id
/// does not match what this build derives -- which is exactly the situation
/// that makes a game a migration candidate in the first place. Routing
/// through the checking path here would refuse the very games this function
/// exists to rescue.
pub async fn migrate_label<N: NodeClient>(
    node: &mut N,
    label: &str,
    contract_wasm: Vec<u8>,
) -> anyhow::Result<MigrateOutcome> {
    let summary = bound_game(node, label).await?;
    let params = summary
        .params
        .clone()
        .expect("bound_game checked this is Some");
    let old = summary.contract.expect("bound_game checked this is Some");

    let (container, new_id) = contract_container(contract_wasm, &params)?;
    if *new_id == old {
        return Ok(MigrateOutcome::AlreadyCurrent { contract: old });
    }

    // Read the game from where it lives now. Scoped deliberately: if the old
    // contract has gone cold there is no local copy to fall back on, and
    // saying so is better than PUTting an empty state over the new address.
    let raw = node
        .get(ContractInstanceId::new(old), false)
        .await
        .context("GET the old contract")?
        .ok_or_else(|| {
            anyhow!(
                "the previous contract {} is no longer on the network, so this \
                 game cannot be migrated",
                ContractInstanceId::new(old).encode()
            )
        })?;
    let state = if raw.is_empty() {
        GameState::empty()
    } else {
        GameState::decode(&raw)
            .ok_or_else(|| anyhow!("the previous contract's state did not decode"))?
    };
    let records = state.records.len();

    node.put(container, state.encode())
        .await
        .context("PUT the game under the new contract id")?;

    match node
        .delegate(Request::Rebind {
            label: label.to_string(),
            contract: *new_id,
        })
        .await
        .context("Rebind")?
    {
        Response::Bound { .. } => {}
        Response::Refused(r) => return Err(refused(r)),
        other => bail!("unexpected response to Rebind: {other:?}"),
    }
    Ok(MigrateOutcome::Migrated {
        from: old,
        to: *new_id,
        records,
    })
}

/// Derive the contract's key and container from `params`, and confirm the id
/// matches what the delegate recorded at bind time. A mismatch means this
/// build's `contract_wasm` differs from the one used when the game was
/// bound, which would otherwise silently GET, PUT and UPDATE the wrong
/// contract.
fn expected_container(
    label: &str,
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
             version used when this game was bound, or run \
             `adjourn game migrate --label {label}` to move this game onto \
             the contract this build derives.",
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

/// Everything a screen needs about one bound game.
///
/// `state` is here as well as `status` because `Status.chain` carries record
/// IDs, not moves: rendering a move history means looking those IDs up in the
/// record set. Nothing else needs the raw state.
#[derive(Clone, Debug)]
pub struct GameView {
    pub label: String,
    pub side: Side,
    pub params: GameParams,
    pub game_id: [u8; 32],
    pub contract: [u8; 32],
    pub state: GameState,
    pub status: Status,
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
    view: GameView,
    container: ContractContainer,
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
    let container = expected_container(label, contract_wasm, &params, contract)?;

    let state = fetch_state(node, contract).await?;
    let status = project(&state, &params);

    Ok(OpenGame {
        view: GameView {
            label: label.to_string(),
            side,
            params,
            game_id,
            contract,
            state,
            status,
        },
        container,
    })
}

/// The public half of `open_game`, for screens that only need to render.
pub async fn open_game_view<N: NodeClient>(
    node: &mut N,
    label: &str,
    contract_wasm: Vec<u8>,
) -> anyhow::Result<GameView> {
    Ok(open_game(node, label, contract_wasm).await?.view)
}

/// The accepted moves, in play order, as UCI.
///
/// Driven by `status.chain` and NOT by iterating `state.records`: the record
/// set is a `BTreeMap` keyed by ID, so iterating it yields hash order, which
/// has nothing to do with the order the moves were played.
///
/// The inner `?` in the `filter_map` below silently skips a chain id that is
/// missing from `view.state.records`, rather than reporting it. That is only
/// safe because `view.status` and `view.state` are always produced by one
/// `project(&state, ...)` call over the same `state` -- every id `status.chain`
/// names is, by construction, a record in that same `state`. It stops being
/// safe the moment a caller holds a `status` from one merge and a `state`
/// from an earlier one: exactly the bug `watch_label`'s callback used to
/// invite, when the UI updated `status` on every notification but left
/// `state` frozen at whatever the initial open returned. The fix there was to
/// keep the pair moving together (see `watch_label`'s callback signature),
/// not to make this function tolerate the two drifting apart -- a `GameView`
/// with a `status`/`state` mismatch is a bug at the call site, and this
/// function has no way to tell "the game legitimately doesn't have that
/// record yet" from "someone handed me two different snapshots", so silently
/// dropping the id is the least-wrong of the two options it can't
/// distinguish. If this ever fires in practice, look at the caller, not here.
pub fn moves_in_order(view: &GameView) -> Vec<String> {
    view.status
        .chain
        .iter()
        .filter_map(|id| match &view.state.records.get(id)?.body {
            Body::Move { uci, .. } => Some(uci.clone()),
            _ => None,
        })
        .collect()
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
    Ok(g.view.status)
}

/// Decode an update notification's `State` half. `Ok(None)` means the payload
/// was empty and there is nothing to merge.
///
/// Both this and [`decode_delta_payload`] REPORT a genuine decode failure
/// rather than swallowing it. A dropped decode error leaves a board that
/// silently never updates with no error anywhere -- exactly the symptom of
/// decoding a `Delta` as a `GameState` or vice versa, which is why the two
/// are separate functions over separate payload halves.
///
/// But an EMPTY payload is not a failure, and erroring on one would exit
/// `adjourn watch` non-zero mid-game over a perfectly legal broadcast. Empty
/// state means "I have nothing", not "I am malformed" -- a contract is PUT
/// with `Vec::new()` before either player moves, and that PUT is broadcast to
/// subscribers verbatim. `GameState::decode` returns `None` on zero bytes, so
/// the empty case has to be split out here rather than left to it.
fn decode_state_payload(bytes: &[u8]) -> anyhow::Result<Option<GameState>> {
    if bytes.is_empty() {
        return Ok(None);
    }
    GameState::decode(bytes).map(Some).ok_or_else(|| {
        anyhow::anyhow!("an update notification's State payload did not decode as a GameState")
    })
}

/// Decode an update notification's `Delta` half. A `Delta` is `Vec<Record>` --
/// a DIFFERENT type with a different encoding from `GameState`, not an
/// interchangeable one.
///
/// `Ok(None)` on empty bytes, for the same reason as [`decode_state_payload`]
/// and one more specific to deltas: `get_state_delta` deliberately emits ZERO
/// bytes for an empty delta rather than an encoded empty list, so that
/// freenet-core's "empty delta -> skip broadcast" path can fire at all. That
/// is the `self_delta_empty` fix this repo already made, and treating those
/// zero bytes as corruption here would undo its point at the receiving end.
fn decode_delta_payload(bytes: &[u8]) -> anyhow::Result<Option<Delta>> {
    if bytes.is_empty() {
        return Ok(None);
    }
    ciborium::from_reader::<Delta, &[u8]>(bytes)
        .map(Some)
        .map_err(|e| anyhow::anyhow!("an update notification's Delta payload did not decode: {e}"))
}

/// Follow a game, calling `on_status` with the projection after every update.
///
/// Merges each notification into the held state rather than replacing it: the
/// payload may be a `State`, a `Delta`, or a `StateAndDelta`, and merge is what
/// makes all three land on the same answer regardless of arrival order.
/// Unrecognised `UpdateData` variants are ignored -- the enum is
/// `#[non_exhaustive]`, and a panic here would end a healthy session.
pub async fn watch_label<N: NodeClient>(
    node: &mut N,
    label: &str,
    contract_wasm: Vec<u8>,
    mut on_status: impl FnMut(&GameState, &Status),
) -> anyhow::Result<()> {
    let g = open_game(node, label, contract_wasm).await?;
    let mut state = g.view.state;
    on_status(&state, &g.view.status);
    // A decided game will never produce another notification, so entering the
    // loop would block forever on a game that has already ended -- printing
    // the final position and then hanging.
    if g.view.status.is_over() {
        return Ok(());
    }

    // `open_game`'s GET deliberately does not subscribe -- the one-shot
    // commands share it and must not leave subscriptions behind. Watching is
    // the one flow that needs one, so it asks for its own.
    //
    // Its returned state is MERGED, not discarded. Between `open_game`'s
    // non-subscribing GET and the subscription landing there is a window in
    // which the opponent's move is broadcast to subscribers we are not yet
    // among -- lost entirely, leaving a terminal on a stale position forever,
    // indistinguishable from an idle game. This GET's answer is the freshest
    // view available and closes that window on any transport, whatever the
    // transport's own subscribe-ordering guarantees are.
    let subscribed = node
        .get(ContractInstanceId::new(g.view.contract), true)
        .await?;
    // Same empty-is-not-malformed rule as the notification arms below, and
    // through the same helper so the two can never drift apart.
    let fresh = match subscribed.as_deref() {
        Some(bytes) => decode_state_payload(bytes)?,
        None => None,
    };
    if let Some(fresh) = fresh {
        let before = state.records.len();
        state.merge(&fresh, &g.view.params);
        // Report only if that actually moved us on, so the common case (the
        // two GETs agree) does not render the same position twice.
        if state.records.len() != before {
            let status = project(&state, &g.view.params);
            on_status(&state, &status);
            if status.is_over() {
                return Ok(());
            }
        }
    }

    loop {
        // A real `WsClient` blocks until a notification arrives and can never
        // return `None` (see the `NodeClient::next_update` doc), so `None`
        // means a fake or a closed stream with nothing more to deliver.
        // Returning is the only correct answer: `continue` here is dead code
        // against the real transport and a yield-free hot loop against
        // `FakeNode`, which wedges a current-thread runtime.
        let Some((id, update)) = node.next_update().await? else {
            return Ok(());
        };
        // `OpenGame.contract` is a raw `[u8; 32]`; `ContractInstanceId` derefs
        // to the same, so compare through the deref rather than by type.
        if *id != g.view.contract {
            continue; // a different game on the same connection
        }
        // A `State` payload is an encoded `GameState`; a `Delta` payload is an
        // encoded `Delta`, which is `Vec<Record>` -- a DIFFERENT type with a
        // different encoding. Decoding one as the other fails silently and the
        // board simply never updates, so the two arms must not be merged.
        //
        // A decode failure in any arm is REPORTED, not swallowed. Dropping it
        // leaves a board that silently never updates with no error anywhere --
        // exactly the symptom that would follow from decoding a `Delta` as a
        // `GameState` or vice versa, and the reason the arms are kept apart.
        // An EMPTY payload is not a decode failure and must not be treated as
        // one -- see `decode_state_payload` and `decode_delta_payload`.
        match update {
            UpdateData::State(bytes) => {
                if let Some(incoming) = decode_state_payload(bytes.as_ref())? {
                    state.merge(&incoming, &g.view.params);
                }
            }
            UpdateData::Delta(bytes) => {
                if let Some(delta) = decode_delta_payload(bytes.as_ref())? {
                    state.apply_delta(&delta, &g.view.params);
                }
            }
            UpdateData::StateAndDelta { state: s, delta } => {
                if let Some(incoming) = decode_state_payload(s.as_ref())? {
                    state.merge(&incoming, &g.view.params);
                }
                if let Some(delta) = decode_delta_payload(delta.as_ref())? {
                    state.apply_delta(&delta, &g.view.params);
                }
            }
            // `UpdateData` is `#[non_exhaustive]`. Ignore what we do not know.
            _ => continue,
        }
        let status = project(&state, &g.view.params);
        on_status(&state, &status);
        if status.is_over() {
            return Ok(());
        }
    }
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

    if g.view.status.is_over() {
        bail!("the game is already over");
    }
    if Side::from(g.view.status.turn) != g.view.side {
        bail!("it is not your turn");
    }
    if !legal_moves(&g.view.state, &g.view.params)
        .iter()
        .any(|m| m == uci)
    {
        bail!("{uci} is not a legal move in the current position");
    }

    let parent = g
        .view
        .status
        .chain
        .last()
        .copied()
        .unwrap_or_else(|| g.view.params.genesis());
    let body = Body::Move {
        ply: g.view.status.ply + 1,
        parent,
        uci: uci.to_string(),
    };

    sign_and_submit(
        node,
        g.view.game_id,
        g.container,
        &g.view.params,
        g.view.contract,
        body,
    )
    .await
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
    if g.view.status.is_over() {
        bail!("the game is already over");
    }

    sign_and_submit(
        node,
        g.view.game_id,
        g.container,
        &g.view.params,
        g.view.contract,
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
    if g.view.status.is_over() {
        bail!("the game is already over");
    }
    let at = g
        .view
        .status
        .chain
        .last()
        .copied()
        .unwrap_or_else(|| g.view.params.genesis());

    sign_and_submit(
        node,
        g.view.game_id,
        g.container,
        &g.view.params,
        g.view.contract,
        Body::DrawOffer {
            ply: g.view.status.ply,
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
    if g.view.status.is_over() {
        bail!("the game is already over");
    }
    let head = g
        .view
        .status
        .chain
        .last()
        .copied()
        .unwrap_or_else(|| g.view.params.genesis());
    let our_color: Color = g.view.side.into();

    let offer = g
        .view
        .state
        .records
        .iter()
        .find(|(_, rec)| {
            matches!(&rec.body, Body::DrawOffer { at, .. } if *at == head)
                && rec.color(&g.view.params) == Some(!our_color)
        })
        .map(|(id, _)| *id)
        .ok_or_else(|| anyhow::anyhow!("no live draw offer from your opponent to accept"))?;

    sign_and_submit(
        node,
        g.view.game_id,
        g.container,
        &g.view.params,
        g.view.contract,
        Body::DrawAccept {
            ply: g.view.status.ply,
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
    if g.view.status.is_over() {
        bail!("the game is already over");
    }
    let our_color: Color = g.view.side.into();
    if g.view.status.turn != our_color {
        bail!("only the player to move may claim a draw");
    }
    if g.view.status.repetitions < 3 && g.view.status.halfmove_clock < 100 {
        bail!(
            "no draw to claim: {} repetitions, {} halfmoves since a capture or pawn move",
            g.view.status.repetitions,
            g.view.status.halfmove_clock
        );
    }
    let at = g
        .view
        .status
        .chain
        .last()
        .copied()
        .unwrap_or_else(|| g.view.params.genesis());

    sign_and_submit(
        node,
        g.view.game_id,
        g.container,
        &g.view.params,
        g.view.contract,
        Body::DrawClaim {
            ply: g.view.status.ply,
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
    let parent =
        if ply <= 1 {
            g.view.params.genesis()
        } else {
            *g.view.status.chain.get(ply as usize - 2).ok_or_else(|| {
                anyhow::anyhow!("no record at ply {} to parent ply {ply} on", ply - 1)
            })?
        };
    let body = Body::Move {
        ply,
        parent,
        uci: uci.to_string(),
    };

    sign_and_submit(
        node,
        g.view.game_id,
        g.container,
        &g.view.params,
        g.view.contract,
        body,
    )
    .await
    .with_context(|| format!("sign move at ply {ply}"))
}
