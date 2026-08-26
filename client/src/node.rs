//! The node seam.
//!
//! `WsClient` (see the CLI's `ws.rs`) is the real thing. `FakeNode` (see
//! `fake.rs`) is the other impl, and it runs the real contract and delegate
//! code so CI can exercise the session flows without a Freenet node.

use adjourn_core::delegate_api::{Request, Response};
use adjourn_core::GameParams;
use anyhow::Context;
use freenet_stdlib::prelude::*;
use std::sync::Arc;

// `NodeClient` is only ever used generically (`<N: NodeClient>`), never as
// `dyn NodeClient`, so the missing auto-trait bounds this lint warns about
// (e.g. `Send` on the returned future) are not a real hazard here.
#[allow(async_fn_in_trait)]
pub trait NodeClient {
    /// `Ok(None)` means the node does not have this contract.
    async fn get(
        &mut self,
        id: ContractInstanceId,
        subscribe: bool,
    ) -> anyhow::Result<Option<Vec<u8>>>;
    async fn put(&mut self, container: ContractContainer, state: Vec<u8>) -> anyhow::Result<()>;
    async fn update(&mut self, key: ContractKey, delta: Vec<u8>) -> anyhow::Result<()>;
    async fn delegate(&mut self, req: Request) -> anyhow::Result<Response>;

    /// The next update notification for a contract this client subscribed to.
    ///
    /// `Ok(None)` means "nothing waiting" for `FakeNode`, which drains a
    /// shared in-memory log and returns once it is empty. `WsClient` has no
    /// such log to exhaust — it blocks on the socket's `recv()` — so for a
    /// real node this call can never return `None`; it either yields an
    /// update or does not return.
    ///
    /// Deliberately NOT bounded by a request timeout. A correspondence move can
    /// legitimately take days, so a timeout here would report a healthy idle
    /// game as a failure — the opposite of what the per-request timeout on the
    /// other methods is for.
    ///
    /// The payload is `UpdateData`, which may be a `State`, a `Delta`, or a
    /// `StateAndDelta` — the notification does not promise which. Callers hold
    /// a `GameState` and MERGE whatever arrives rather than replacing, which is
    /// what makes arrival order irrelevant and lets a browser converge exactly
    /// as a peer does. `UpdateData` is `#[non_exhaustive]`: ignore variants you
    /// do not recognise rather than panicking on them.
    async fn next_update(
        &mut self,
    ) -> anyhow::Result<Option<(ContractInstanceId, UpdateData<'static>)>>;
}

/// Build the contract container and its instance id from raw cargo WASM.
///
/// `fdev publish` wants a pre-packaged file, but the programmatic path takes
/// the raw module and applies the version wrapper itself — this is what River
/// does, and it is why `scripts/build-contract.sh` output is the right
/// artifact.
pub fn contract_container(
    wasm: Vec<u8>,
    params: &GameParams,
) -> anyhow::Result<(ContractContainer, ContractInstanceId)> {
    let mut param_bytes = Vec::new();
    ciborium::into_writer(params, &mut param_bytes).context("encode params")?;
    let parameters = Parameters::from(param_bytes);
    let code = ContractCode::from(wasm);
    let id = ContractInstanceId::from_params_and_code(&parameters, &code);
    let container = ContractContainer::from(ContractWasmAPIVersion::V1(WrappedContract::new(
        Arc::new(code),
        parameters,
    )));
    Ok((container, id))
}

/// The delegate key is a pure function of its code, so it is derived rather
/// than stored — nothing to keep in sync, and no stale cached key pointing at
/// a generation that is gone.
pub fn delegate_container(wasm: Vec<u8>) -> (DelegateContainer, DelegateKey) {
    let code = DelegateCode::from(wasm);
    let params = Parameters::from(Vec::<u8>::new());
    let delegate = Delegate::from((&code, &params));
    let key = delegate.key().clone();
    (
        DelegateContainer::Wasm(DelegateWasmAPIVersion::V1(delegate)),
        key,
    )
}
