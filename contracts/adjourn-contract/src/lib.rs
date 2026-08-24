//! The Freenet contract wrapper.
//!
//! All the interesting logic lives in `adjourn-core`, which has no Freenet
//! dependencies so the algebra can be tested standalone. This crate is only the
//! adapter: bytes in, bytes out.
//!
//! | contract method    | core                     |
//! |--------------------|--------------------------|
//! | `validate_state`   | `GameState::all_valid`   |
//! | `update_state`     | `GameState::merge`       |
//! | `summarize_state`  | `GameState::summarize`   |
//! | `get_state_delta`  | `GameState::delta_against` |

use adjourn_core::state::{Delta, Summary};
use adjourn_core::{GameParams, GameState};
use ciborium::{de::from_reader, ser::into_writer};
use freenet_stdlib::prelude::*;

/// Empty bytes mean "nothing yet", not "malformed".
///
/// A contract is PUT before either player has moved, and peers ask for a
/// summary or a delta against a state they have only just created. Treating
/// empty as a decode error would make a fresh game unreachable.
fn decode_state(bytes: &[u8]) -> Result<GameState, ContractError> {
    if bytes.is_empty() {
        return Ok(GameState::empty());
    }
    from_reader::<GameState, &[u8]>(bytes).map_err(|e| ContractError::Deser(e.to_string()))
}

fn decode_params(parameters: &Parameters<'static>) -> Result<GameParams, ContractError> {
    from_reader::<GameParams, &[u8]>(parameters.as_ref())
        .map_err(|e| ContractError::Deser(e.to_string()))
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, ContractError> {
    let mut buf = Vec::new();
    into_writer(value, &mut buf).map_err(|e| ContractError::Deser(e.to_string()))?;
    Ok(buf)
}

/// The validity predicate, in one place so `validate_state` and `update_state`
/// cannot drift apart.
///
/// Structural only: is every record a real statement by one of the two players?
/// It deliberately does NOT check chess legality. An illegal move is a
/// well-formed signed statement that the projection ignores — if illegality
/// made the whole state invalid, either player could destroy the game by
/// signing one garbage move, and every honest peer would reject the state.
fn check_valid(state: &GameState, params: &GameParams) -> Result<(), ContractError> {
    if state.all_valid(params) {
        Ok(())
    } else {
        Err(ContractError::InvalidUpdateWithInfo {
            reason: "state contains a record not signed by either player".into(),
        })
    }
}

/// Public so the interface can be exercised directly from integration tests
/// without standing up a Freenet node.
pub struct Contract;

#[contract]
impl ContractInterface for Contract {
    fn validate_state(
        parameters: Parameters<'static>,
        state: State<'static>,
        _related: RelatedContracts<'static>,
    ) -> Result<ValidateResult, ContractError> {
        let bytes = state.as_ref();
        if bytes.is_empty() {
            return Ok(ValidateResult::Valid);
        }
        let params = decode_params(&parameters)?;
        let state = decode_state(bytes)?;

        // Chess legality is NOT checked here. See `check_valid`.
        if state.all_valid(&params) {
            Ok(ValidateResult::Valid)
        } else {
            Ok(ValidateResult::Invalid)
        }
    }

    fn update_state(
        parameters: Parameters<'static>,
        state: State<'static>,
        data: Vec<UpdateData<'static>>,
    ) -> Result<UpdateModification<'static>, ContractError> {
        let params = decode_params(&parameters)?;
        let mut game = decode_state(state.as_ref())?;

        for update in data {
            match update {
                UpdateData::State(incoming) => {
                    let incoming = decode_state(incoming.as_ref())?;
                    check_valid(&incoming, &params)?;
                    game.merge(&incoming, &params);
                }
                UpdateData::Delta(d) => {
                    if d.as_ref().is_empty() {
                        continue;
                    }
                    let delta: Delta = from_reader::<Delta, &[u8]>(d.as_ref())
                        .map_err(|e| ContractError::Deser(e.to_string()))?;
                    // Verify every record before absorbing any: `apply_delta`
                    // absorbs all then evicts ONCE, which is what keeps this
                    // an O(n) pass rather than the O(n^2 log n) that calling
                    // `insert_verified` (evict-per-record) in a loop would be.
                    for rec in &delta {
                        if !rec.verify(&params) {
                            return Err(ContractError::InvalidUpdateWithInfo {
                                reason: "delta contains a record not signed by either player"
                                    .into(),
                            });
                        }
                    }
                    game.apply_delta(&delta, &params);
                }
                // Both halves of a two-step sync can arrive together.
                UpdateData::StateAndDelta { state, delta } => {
                    let incoming = decode_state(state.as_ref())?;
                    check_valid(&incoming, &params)?;
                    game.merge(&incoming, &params);

                    if !delta.as_ref().is_empty() {
                        let delta: Delta = from_reader::<Delta, &[u8]>(delta.as_ref())
                            .map_err(|e| ContractError::Deser(e.to_string()))?;
                        // Same batching as the `Delta` arm above: verify first,
                        // then one absorb-all-then-evict-once pass.
                        for rec in &delta {
                            if !rec.verify(&params) {
                                return Err(ContractError::InvalidUpdateWithInfo {
                                    reason: "delta contains a record not signed by either player"
                                        .into(),
                                });
                            }
                        }
                        game.apply_delta(&delta, &params);
                    }
                }
                // This game has no related contracts: params are exchanged out
                // of band and the two keys are baked into the contract key.
                UpdateData::RelatedState { .. }
                | UpdateData::RelatedDelta { .. }
                | UpdateData::RelatedStateAndDelta { .. } => {}
                // `UpdateData` is `#[non_exhaustive]`. Reject unknown variants
                // rather than panicking: a panic inside contract WASM kills the
                // runtime for this contract and surfaces as an opaque execution
                // error, whereas `InvalidUpdate` is recoverable and diagnosable.
                _ => return Err(ContractError::InvalidUpdate),
            }
        }

        Ok(UpdateModification::valid(encode(&game)?.into()))
    }

    fn summarize_state(
        parameters: Parameters<'static>,
        state: State<'static>,
    ) -> Result<StateSummary<'static>, ContractError> {
        let params = decode_params(&parameters)?;
        let game = decode_state(state.as_ref())?;
        // Normalize before summarizing: a peer can PUT a crafted over-K state
        // (validate_state is permissive by design), and without this an
        // evicted-away record would summarize forever and its delta would
        // re-offer forever -- the same never-settles shape as the encoded-
        // empty-delta bug below in `get_state_delta`. `filter_valid` verifies
        // then evicts, so a forged low-id record cannot evict an honest one.
        let game = game.filter_valid(&params);
        Ok(StateSummary::from(encode(&game.summarize())?))
    }

    fn get_state_delta(
        parameters: Parameters<'static>,
        state: State<'static>,
        summary: StateSummary<'static>,
    ) -> Result<StateDelta<'static>, ContractError> {
        let params = decode_params(&parameters)?;
        let game = decode_state(state.as_ref())?;
        // See `summarize_state`: normalize a possibly-crafted over-K state
        // before diffing against it, so an evicted-away record is not offered
        // forever.
        let game = game.filter_valid(&params);

        // An empty summary means the peer holds nothing, so it needs everything
        // — NOT that it is already up to date.
        let summary: Summary = if summary.as_ref().is_empty() {
            Summary::new()
        } else {
            from_reader::<Summary, &[u8]>(summary.as_ref())
                .map_err(|e| ContractError::Deser(e.to_string()))?
        };

        let delta = game.delta_against(&summary);

        // A peer that is already up to date must get ZERO bytes, not a
        // CBOR-encoded empty list -- which is one byte, `0x80`.
        //
        // freenet-core decides whether to skip a broadcast on the ENCODED
        // length, so a delta that is never empty keeps the "empty delta ->
        // skip" path from ever firing and lets two peers re-offer to each
        // other indefinitely. That is what drove River's 2026-07-25 bandwidth
        // incident. `fdev conformance` flags it as `self_delta_empty`.
        //
        // `update_state` already treats empty delta bytes as a no-op, so the
        // two halves stay symmetric.
        if delta.is_empty() {
            return Ok(StateDelta::from(Vec::new()));
        }

        Ok(StateDelta::from(encode(&delta)?))
    }
}
